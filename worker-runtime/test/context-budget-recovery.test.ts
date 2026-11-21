import { Database } from "bun:sqlite";
import { afterEach, describe, expect, it, mock } from "bun:test";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import type { SqliteDatabase, SqliteExecutor, SqliteValue } from "@earendil-works/chord/sqlite";
import { Harness } from "@earendil-works/pi-durable";
import { SqliteStorage } from "@earendil-works/pi-durable/storage/sqlite";
import { BOARD_MESSAGE_KIND, compactionFailureText, type GenerationIdentity, type ThreadEntry } from "../src/contract.ts";
import { BYTE_COMPACTION_THRESHOLD, CONTEXT_BUDGET_EXCEEDED_TEXT, defaultModel, HOSTED_COMPACTION_POLICY, SUMMARY_MAX_OUTPUT_TOKENS } from "../src/model.ts";
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
    async runCounted(sql: string, ...params: SqliteValue[]) {
      return Number(database.prepare(sql).run(...(params as never[])).changes);
    },
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
/** An assistant turn that writes an artifact: the HTML travels as a tool-call argument. */
function putCall(html: string) {
  return new Response(
    chunk({ role: "assistant", content: "" }) +
      chunk({ tool_calls: [{ index: 0, id: `put-${html.length}`, type: "function",
        function: { name: "put_artifact", arguments: JSON.stringify({ html }) } }] }) +
      chunk({}, "tool_calls") + "data: [DONE]\n\n",
    { headers: { "content-type": "text/event-stream" } },
  );
}
const lastRole = (input: Record<string, unknown>) =>
  (input.messages as Array<{ role?: string }>).at(-1)?.role;
function cutOff(text: string) {
  return new Response(
    chunk({ role: "assistant", content: "" }) + chunk({ content: text }) +
      chunk({}, "length") + "data: [DONE]\n\n",
    { headers: { "content-type": "text/event-stream" } },
  );
}
/** The engine's summarization request opens with its own system prompt. */
const isSummary = (input: Record<string, unknown>) => {
  const first = (input.messages as Array<{ role?: string; content?: unknown }>)[0];
  return first?.role === "system" && JSON.stringify(first.content).includes("context summarization assistant");
};
const outputBudget = (input: Record<string, unknown>) =>
  input.max_completion_tokens ?? input.max_tokens;
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
  options: { keepRecentTokens?: number; artifact?: string; reserveTokens?: number; put?: boolean; backgroundTokens?: number } = {},
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
  const id = { name: "conversation", toString: () => "test-object-id" };
  const requests: RequestRecord[] = [];
  let toolExecutions = 0;
  const hosted = new HostedSession(
    { id, storage: { sql, get: async () => undefined } } as never,
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
      tools: options.artifact === undefined && !options.put ? [] : [{
        name: "artifacts", tools: [...(options.put ? [{
          name: "put_artifact", description: "Write the HTML artifact.",
          parameters: {
            type: "object", properties: { html: { type: "string" } },
            required: ["html"], additionalProperties: false,
          },
          replay: "safe",
          async execute() { return { output: [{ type: "text", text: "Saved." }] }; },
        }] : []), ...(options.artifact === undefined ? [] : [{
          name: "read_artifact", description: "Read the HTML artifact.",
          parameters: { type: "object", properties: {}, additionalProperties: false },
          replay: "safe",
          async execute() {
            toolExecutions++;
            return { output: [{ type: "text", text: options.artifact! }] };
          },
        }])],
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
        compaction: { backgroundTokens: options.backgroundTokens ?? 0, keepRecentTokens: options.keepRecentTokens ?? 20_000, ...(options.reserveTokens === undefined ? {} : { reserveTokens: options.reserveTokens }) },
      }),
    },
    BACKGROUND_CONTEXT,
  );
  cleanups.push(async () => { await harness.close(BACKGROUND_CONTEXT); database.close(); });
  const conversation = await harness.root(BACKGROUND_CONTEXT, {
    agent: { model: { provider: defaultModel("default").provider, modelId: "default" } },
  });
  wired.harness = {
    pi: async () => harness,
    session: () => ({ id: conversation.id, watch: () => conversation.watch(BACKGROUND_CONTEXT) }),
    conversation: async () => conversation,
    pending: async () => [],
  };
  let operations = 0;
  return {
    hosted, requests, toolExecutions: () => toolExecutions,
    reset: () => conversation.reset(undefined, BACKGROUND_CONTEXT),
    async writeRecord(entry: Record<string, unknown>) {
      const operation = await conversation.submit({ type: "write", entry } as never, BACKGROUND_CONTEXT);
      await operation.wait(BACKGROUND_CONTEXT);
    },
    inspectReconstructed: (readSql: typeof sql = sql) => (Object.assign(Object.create(HostedSession.prototype), {
      ctx: { id, storage: { sql: readSql } },
    }) as InstanceType<typeof HostedSession>).sessionSummary(),
    unavailableTranscript: (sessionId: string) => (Object.assign(Object.create(HostedSession.prototype), {
      ctx: { id, storage: { sql: { exec() { throw new Error("SQL unavailable"); } } } },
    }) as InstanceType<typeof HostedSession>).sessionTranscript(sessionId, 0),
    async submit(content: string) {
      const id = `operation-${++operations}`;
      return conversation.submit({
        type: "input", requestId: id, content,
        entry: { kind: BOARD_MESSAGE_KIND, data: { messageId: id } },
      }, BACKGROUND_CONTEXT);
    },
  };
}

describe("hosted session summaries", () => {
  it("keeps a whole generation across operations and eviction, and closes it only on reset", async () => {
    const run = await session(() => answer("private answer"));
    expect(run.hosted.sessionSummary()).toEqual({ state: "unobserved" });
    await (await run.submit("private first prompt")).wait(BACKGROUND_CONTEXT);
    const first = run.hosted.sessionSummary();
    if (first.state !== "observed") throw new Error("expected first session");
    expect(first.session.startedAt).toBeGreaterThan(0);
    await (await run.submit("private second prompt")).wait(BACKGROUND_CONTEXT);
    const beforeReset = run.hosted.sessionSummary();
    if (beforeReset.state !== "observed") throw new Error("expected same session");
    expect(beforeReset.session).toEqual({
      ...first.session,
      lastActivityAt: expect.any(Number),
      status: "open",
      endedAt: null,
      lastRunFailureCode: null,
    });
    expect(beforeReset.session.lastActivityAt!).toBeGreaterThanOrEqual(first.session.lastActivityAt!);
    expect(run.inspectReconstructed()).toEqual(beforeReset);
    expect(run.hosted.sessionSummary("different-object:0")).toEqual({ state: "unobserved" });

    await run.reset();
    const fresh = run.hosted.sessionSummary();
    if (fresh.state !== "observed") throw new Error("expected reset generation");
    expect(fresh.session.sessionId).not.toBe(first.session.sessionId);
    expect(fresh.session.startedAt).toBeNull(); // No timestamp on a bare reset.
    expect(run.hosted.sessionSummary(first.session.sessionId)).toEqual({
      state: "observed",
      session: { ...beforeReset.session, status: "reset", endedAt: null },
    });
    await (await run.submit("private fresh prompt")).wait(BACKGROUND_CONTEXT);
    const active = run.hosted.sessionSummary();
    if (active.state !== "observed") throw new Error("expected fresh session");
    expect(active.session.sessionId).toBe(fresh.session.sessionId);
    expect(active.session.startedAt).toBeGreaterThan(0);
    expect(JSON.stringify([beforeReset, active])).not.toContain("private");
  });

  it("reports a safe run failure without calling the whole session failed", async () => {
    const run = await session(() => Response.json({
      error: { message: "private gateway diagnostics" },
    }, { status: 403 }));
    await (await run.submit("private input")).wait(BACKGROUND_CONTEXT);
    expect(run.hosted.sessionSummary()).toEqual({
      state: "observed",
      session: {
        sessionId: expect.any(String),
        startedAt: expect.any(Number),
        lastActivityAt: expect.any(Number),
        status: "open",
        endedAt: null,
        lastRunFailureCode: "model_error",
      },
    });
  });

  it("keeps an unavailable summary read distinct from no execution evidence", async () => {
    const run = await session(() => answer("answer"));
    expect(run.inspectReconstructed({ exec() { throw new Error("SQL unavailable"); } }))
      .toEqual({ state: "unknown" });
    await (await run.submit("question")).wait(BACKGROUND_CONTEXT);
    expect(run.requests).toHaveLength(1);
    expect(run.hosted.sessionSummary().state).toBe("observed");
  });
});

describe("exact session transcripts", () => {
  it("reads only the requested generation across consecutive resets and rejects foreign cursors", async () => {
    const run = await session(() => answer("visible answer"));
    await (await run.submit("first generation")).wait(BACKGROUND_CONTEXT);
    const first = run.hosted.sessionSummary();
    if (first.state !== "observed") throw new Error("expected first session");
    for (let i = 0; i < 40; i++) await run.writeRecord({
      kind: BOARD_MESSAGE_KIND, data: { messageId: `message-${i}` },
      model: [{ role: "user", content: `first-${i}`, timestamp: 1000 }],
    });
    const page = run.hosted.sessionTranscript(first.session.sessionId, 0);
    expect(page?.nextCursor).toEqual(expect.any(Number));
    await run.writeRecord({
      kind: BOARD_MESSAGE_KIND, data: { messageId: "late" },
      model: [{ role: "user", content: "first-late", timestamp: 1000 }],
    });
    await run.reset();
    const empty = run.hosted.sessionSummary();
    if (empty.state !== "observed") throw new Error("expected empty generation");
    await run.reset();
    await (await run.submit("second generation")).wait(BACKGROUND_CONTEXT);
    const second = run.hosted.sessionSummary();
    if (second.state !== "observed") throw new Error("expected second session");

    const all = [...page!.messages];
    let cursor = page!.nextCursor;
    while (cursor !== null) {
      const next = run.hosted.sessionTranscript(first.session.sessionId, cursor);
      expect(next).not.toBeNull();
      all.push(...next!.messages);
      cursor = next!.nextCursor;
    }
    expect(all.map((message) => message.text)).toEqual([
      "first generation", "visible answer", ...Array.from({ length: 40 }, (_, i) => `first-${i}`), "first-late",
    ]);
    expect(run.hosted.sessionTranscript(empty.session.sessionId, 0)).toEqual({ messages: [], nextCursor: null });
    expect(run.hosted.sessionTranscript(second.session.sessionId, 0)?.messages.map((m) => m.text))
      .toEqual(["second generation", "visible answer"]);
    expect(run.hosted.sessionTranscript(second.session.sessionId, page!.nextCursor!)).toBeNull();
    expect(run.hosted.sessionTranscript(first.session.sessionId, Number.MAX_SAFE_INTEGER)).toBeNull();
    expect(run.hosted.sessionTranscript("foreign:0", 0)).toBeNull();
    expect(run.hosted.sessionTranscript("test-object-id:999999", 0)).toBeNull();
    expect(run.hosted.sessionTranscript(first.session.sessionId, -1)).toBeNull();
    expect(run.requests).toHaveLength(2);
  });

  it("excludes internal prompts, reasoning, tool payloads and compaction while retaining visible text", async () => {
    const run = await session(() => answer("answer"));
    await (await run.submit("question")).wait(BACKGROUND_CONTEXT);
    const summary = run.hosted.sessionSummary();
    if (summary.state !== "observed") throw new Error("expected session");
    for (const [kind, id, text] of [
      [BOARD_MESSAGE_KIND, "routine:123", "private routine"],
      [BOARD_MESSAGE_KIND, "cred-resume:123", "private credential"],
      [BOARD_MESSAGE_KIND, "dispatch:ticket:1", "private dispatch instructions"],
      ["pi.user", "internal", "private instruction"],
      ["pi.compaction", "compaction", "private summary"],
    ]) await run.writeRecord({
      kind, data: { messageId: id }, model: [{ role: "user", content: text, timestamp: 1000 }],
    });
    await run.writeRecord({
      kind: "pi.assistant",
      model: [{ role: "assistant", timestamp: 1000, stopReason: "stop", usage: {},
        content: [{ type: "thinking", thinking: "private thinking" },
          { type: "toolCall", id: "tool", name: "private", arguments: { secret: "private tool" } },
          { type: "text", text: "visible reply" }] }],
    });
    const messages: { role: string; text: string }[] = [];
    let cursor: number | null = 0;
    while (cursor !== null) {
      const page = run.hosted.sessionTranscript(summary.session.sessionId, cursor);
      expect(page).not.toBeNull();
      messages.push(...page!.messages);
      cursor = page!.nextCursor;
    }
    expect(messages.map((message) => message.text)).toEqual(["question", "answer", "visible reply"]);
    expect(JSON.stringify(messages)).not.toContain("private");
  });

  it("pages at the response byte ceiling without splitting or dropping a record", async () => {
    const run = await session(() => answer("answer"));
    await (await run.submit("question")).wait(BACKGROUND_CONTEXT);
    const summary = run.hosted.sessionSummary();
    if (summary.state !== "observed") throw new Error("expected session");
    for (const text of ["a".repeat(30_000), "b".repeat(30_000)]) await run.writeRecord({
      kind: BOARD_MESSAGE_KIND, data: { messageId: text[0] },
      model: [{ role: "user", content: text, timestamp: 1000 }],
    });
    const first = run.hosted.sessionTranscript(summary.session.sessionId, 0);
    expect(first?.messages.map((message) => message.text)).toEqual(["question", "answer", "a".repeat(30_000)]);
    expect(first?.nextCursor).toEqual(expect.any(Number));
    const second = run.hosted.sessionTranscript(summary.session.sessionId, first!.nextCursor!);
    expect(second?.messages.map((message) => message.text)).toEqual(["b".repeat(30_000)]);
    expect(second?.nextCursor).toBeNull();
  });

  it("refuses an atomic record that would exceed the message ceiling", async () => {
    const run = await session(() => answer("answer"));
    await (await run.submit("question")).wait(BACKGROUND_CONTEXT);
    const summary = run.hosted.sessionSummary();
    if (summary.state !== "observed") throw new Error("expected session");
    await run.writeRecord({
      kind: BOARD_MESSAGE_KIND, data: { messageId: "many" },
      model: Array.from({ length: 101 }, () => ({ role: "user", content: "message", timestamp: 1000 })),
    });
    const first = run.hosted.sessionTranscript(summary.session.sessionId, 0);
    expect(first?.messages.map((message) => message.text)).toEqual(["question", "answer"]);
    expect(first?.nextCursor).toEqual(expect.any(Number));
    expect(run.hosted.sessionTranscript(summary.session.sessionId, first!.nextCursor!)).toBeNull();
  });

  it("refuses oversized records and unavailable storage without silently dropping content", async () => {
    const run = await session(() => answer("answer"));
    await (await run.submit("question")).wait(BACKGROUND_CONTEXT);
    const summary = run.hosted.sessionSummary();
    if (summary.state !== "observed") throw new Error("expected session");
    await run.writeRecord({
      kind: BOARD_MESSAGE_KIND, data: { messageId: "large" },
      model: [{ role: "user", content: "x".repeat(100_000), timestamp: 1000 }],
    });
    expect(run.hosted.sessionTranscript(summary.session.sessionId, 0)).toBeNull();
    expect(run.unavailableTranscript(summary.session.sessionId)).toBeNull();
  });
});

const LIMIT = 256 * 1024;
const HTML_CHARS = 261_000;
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
    expect((await run.hosted.state()).compactionFailure).toBe(compactionFailureText("model_error"));
    // The transcript keeps only what the engine recorded: no invented row.
    const thread = await run.hosted.transcript(false);
    expect(failed(thread)).toHaveLength(0);
    expect(JSON.stringify(thread)).not.toContain("private summary failure");
  });

  // CAD-2430: demo-company's long Home conversation failed its first blocking
  // compaction once CAD-2414 made compaction start earlier. A replay of that
  // summary used 6,330 of the 8,192 output tokens, 3,629 of them reasoning.
  it("gives the summary its own output budget and leaves a turn's budget alone", async () => {
    const run = await session(
      (requests) => (requests.length === 2 ? answer("Summary.") : answer("Answer.")),
      { keepRecentTokens: 1, reserveTokens: 1_000_000 },
    );
    await (await run.submit("Earlier work.")).wait(BACKGROUND_CONTEXT);
    await (await run.submit("Continue.")).wait(BACKGROUND_CONTEXT);
    expect(run.requests.map((request) => isSummary(request.input))).toEqual([false, true, false]);
    expect(outputBudget(run.requests[1]!.input)).toBe(SUMMARY_MAX_OUTPUT_TOKENS);
    // A turn keeps whatever the engine sends for it; only the summary is raised.
    expect(outputBudget(run.requests[0]!.input)).not.toBe(SUMMARY_MAX_OUTPUT_TOKENS);
    expect(outputBudget(run.requests[2]!.input)).not.toBe(SUMMARY_MAX_OUTPUT_TOKENS);
    expect((await run.hosted.state()).compactionFailure).toBeNull();
  });

  it("names a summary that ran past its budget, and the next message continues", async () => {
    let summaries = 0;
    const run = await session(
      (requests) => {
        const input = requests.at(-1)!.input;
        if (!isSummary(input)) return answer("Answer.");
        summaries++;
        return cutOff("## Goal\nA summary that never ends");
      },
      { keepRecentTokens: 1, reserveTokens: 1_000_000 },
    );
    await (await run.submit("Earlier work.")).wait(BACKGROUND_CONTEXT);
    await (await run.submit("Continue.")).wait(BACKGROUND_CONTEXT);
    expect(summaries).toBe(1);
    expect((await run.hosted.state()).compactionFailure)
      .toBe(compactionFailureText("output_limit"));
    // Sending again is answered: the engine does not repeat the failed
    // compaction for it, and the failure no longer owns the thread.
    await (await run.submit("Continue.")).wait(BACKGROUND_CONTEXT);
    expect(summaries).toBe(1);
    expect(isSummary(run.requests.at(-1)!.input)).toBe(false);
    const thread = await run.hosted.transcript(false);
    expect(thread.at(-1)?.kind).toBe("turn_result");
    expect(thread.at(-1)?.text).toBe("Answer.");
    expect((await run.hosted.state()).compactionFailure).toBeNull();
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

  it("answers the next chat turn while a passive summary is still running", async () => {
    const summarizing = Promise.withResolvers<void>();
    const finishSummary = Promise.withResolvers<void>();
    const generating = Promise.withResolvers<void>();
    const finishGeneration = Promise.withResolvers<void>();
    let foregroundRequests = 0;
    const run = await session(async (requests) => {
      if (isSummary(requests.at(-1)!.input)) {
        summarizing.resolve();
        await finishSummary.promise;
        return answer("Summary of earlier work.");
      }
      if (++foregroundRequests === 3) {
        generating.resolve();
        await finishGeneration.promise;
      }
      return answer("Acknowledged.");
    }, { keepRecentTokens: 1, reserveTokens: 0, backgroundTokens: 128_001 });
    cleanups.push(() => Promise.resolve(finishSummary.resolve()));
    cleanups.push(() => Promise.resolve(finishGeneration.resolve()));

    await (await run.submit("Earlier work.")).wait(BACKGROUND_CONTEXT);
    const previous = await run.submit("Continue.");
    await summarizing.promise;
    expect((await previous.wait(BACKGROUND_CONTEXT)).status).toBe("done");

    expect(await run.hosted.settledOperation("operation-2")).toMatchObject({ state: "unknown" });
    expect(await run.hosted.settledOperation("operation-2", "chat")).toMatchObject({
      state: "settled", status: "done",
    });
    expect(await run.hosted.settledOperation("operation-3", "chat")).toMatchObject({ state: "absent" });

    const next = await run.submit("One more message.");
    await generating.promise;
    // A passive summary must not mask a simultaneous live foreground request.
    expect(await run.hosted.settledOperation("operation-2", "chat")).toMatchObject({ state: "unknown" });
    expect(await run.hosted.settledOperation("operation-3", "chat")).toMatchObject({ state: "unknown" });
    expect(await run.hosted.settledOperation("operation-4", "chat")).toMatchObject({ state: "unknown" });
    finishGeneration.resolve();
    expect((await next.wait(BACKGROUND_CONTEXT)).status).toBe("done");
    expect(await run.hosted.settledOperation("operation-3", "chat")).toMatchObject({
      state: "settled", status: "done",
    });
    expect(await run.hosted.settledOperation("operation-4", "chat")).toMatchObject({ state: "absent" });
    const thread = await run.hosted.transcript(false);
    expect(thread.filter(entry => entry.role === "operator").map(entry => entry.text)).toEqual([
      "Earlier work.", "Continue.", "One more message.",
    ]);
    expect(thread.at(-1)?.text).toBe("Acknowledged.");
  });

  // CAD-2480: a thread of large HTML tool-call arguments is far over the
  // gateway's bytes while the engine's token estimate is still under its
  // threshold (JSON-in-JSON escaping quadruples every quote the estimate
  // counts once). The hosted policy compacts it before the cap, with no refusal.
  it("compacts before the byte threshold with the token estimate still under", async () => {
    const html = '"'.repeat(11_000);
    const run = await session((requests) => {
      const input = requests.at(-1)!.input;
      if (requests.at(-1)!.bytes > LIMIT) return refused();
      if (isSummary(input)) return answer("Summary of earlier work.");
      return lastRole(input) === "tool" ? answer("Saved the page.") : putCall(html);
    }, { put: true, keepRecentTokens: 8_000, reserveTokens: HOSTED_COMPACTION_POLICY.reserveTokens });
    for (let turn = 0; turn < 7; turn++)
      await (await run.submit(`Rewrite the page, version ${turn}.`)).wait(BACKGROUND_CONTEXT);
    const summaries = run.requests.filter((request) => isSummary(request.input));
    expect(summaries.length).toBeGreaterThanOrEqual(1);
    expect(run.requests.filter((request) => request.bytes > LIMIT)).toHaveLength(0);
    const ordinary = run.requests.filter((request) => !isSummary(request.input));
    expect(Math.max(...ordinary.map((request) => request.bytes))).toBeLessThanOrEqual(BYTE_COMPACTION_THRESHOLD);
    expect(failed(await run.hosted.transcript(false))).toHaveLength(0);
    expect((await run.hosted.transcript(false)).at(-1)?.text).toBe("Saved the page.");
  }, 15_000);

  // CAD-2480: the summary's source keeps every tool-call argument whole, so a
  // backlog of large HTML could not be summarised in one request.
  it("summarises a backlog of large HTML arguments in one request under the cap", async () => {
    const run = await session((requests) => {
      const input = requests.at(-1)!.input;
      if (requests.at(-1)!.bytes > LIMIT) return refused();
      if (isSummary(input)) return answer("Summary of earlier work.");
      const messages = input.messages as Array<{ role?: string; content?: unknown }>;
      const asked = JSON.stringify(messages.findLast((message) => message.role === "user")?.content);
      return lastRole(input) !== "tool" && asked.includes("Write the page.")
        ? putCall("<p>" + "x".repeat(HTML_CHARS))
        : answer("Saved the page.");
    }, { put: true, keepRecentTokens: 8_000, reserveTokens: 0 });
    await (await run.submit("Write the page.")).wait(BACKGROUND_CONTEXT);
    await (await run.submit("y".repeat(30_000))).wait(BACKGROUND_CONTEXT);
    const summaries = run.requests.filter((request) => isSummary(request.input));
    expect(summaries).toHaveLength(1);
    expect(summaries[0]!.bytes).toBeLessThan(LIMIT);
    expect(JSON.stringify(summaries[0]!.input)).toContain("more characters omitted");
    expect(failed(await run.hosted.transcript(false))).toHaveLength(0);
    expect((await run.hosted.transcript(false)).at(-1)?.text).toBe("Saved the page.");
  }, 15_000);
});
