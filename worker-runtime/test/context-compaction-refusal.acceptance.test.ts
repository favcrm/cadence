import { Database } from "bun:sqlite";
import { afterEach, describe, expect, it, mock } from "bun:test";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import type { SqliteDatabase, SqliteExecutor, SqliteValue } from "@earendil-works/chord/sqlite";
import { CompactionTask, Harness, hook, watchEvents } from "@earendil-works/pi-durable";
import { SqliteStorage } from "@earendil-works/pi-durable/storage/sqlite";
import type { GenerationIdentity } from "../src/contract.ts";
import { BOARD_MESSAGE_KIND } from "../src/contract.ts";
import { defaultModel } from "../src/model.ts";
import type { ModelTaskContext } from "../src/model-task-context.ts";

// Match the other constructed HostedSession acceptance tests: only the Workers
// lifecycle is stubbed; the session, engine, SQL storage and model binding are real.
mock.module("cloudflare:workers", () => ({
  DurableObject: class {
    constructor(readonly ctx: unknown, readonly env: unknown) {}
  },
}));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

async function within<T>(promise: Promise<T>): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error("Timed out waiting for an engine event")), 10_000);
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

const chunk = (delta: Record<string, unknown>, finish: string | null = null) =>
  `data: ${JSON.stringify({
    id: "synthetic-response",
    object: "chat.completion.chunk",
    created: 1,
    model: "synthetic-model",
    choices: [{ index: 0, delta, finish_reason: finish }],
  })}\n\n`;

function response(text: string): Response {
  return new Response(
    chunk({ role: "assistant", content: "" }) +
      chunk({ content: text }) +
      chunk({}, "stop") +
      "data: [DONE]\n\n",
    { headers: { "content-type": "text/event-stream" } },
  );
}

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

const cleanups: Array<() => Promise<void>> = [];
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup();
});

async function exercise(failCompactionRead: boolean) {
  const { HostedSession } = await import("../src/hosted-session.ts");
  const database = new Database(":memory:");
  const generationEntered = deferred<void>();
  const releaseGeneration = deferred<void>();
  const started = deferred<{ taskId: number; reason: string; blocking: boolean }>();
  const ended = deferred<{ taskId: number; error?: string }>();
  const requests: GenerationIdentity[] = [];
  const compactionReads: number[] = [];
  let active = false;

  // This is the session's read boundary only. The engine uses sqliteFacade
  // below and continues to read and write the real database unmodified.
  const sql = {
    exec(query: string, ...bindings: (string | number | null)[]) {
      if (active && /\bFROM\s+pi_tasks\b/i.test(query) && query.includes('"pi.compaction"')) {
        compactionReads.push(Number(bindings[0]));
        if (failCompactionRead) throw new Error("synthetic compaction identity read failure");
      }
      if (/^\s*(SELECT|PRAGMA|WITH)/i.test(query)) {
        const rows = database.prepare(query).all(...bindings);
        return { toArray: () => rows };
      }
      database.prepare(query).run(...bindings);
      return { toArray: () => [] };
    },
  };

  const session = new HostedSession(
    { id: { name: "conversation" }, storage: { sql } } as never,
    {} as never,
    {
      modelAlias: "default",
      tools: [],
      authorizeTool: async () => {},
      toolRefusal: () => undefined,
      async requestModel(_input: Record<string, unknown>, identity: GenerationIdentity) {
        requests.push({ ...identity });
        if (requests.length === 2) {
          active = true;
          generationEntered.resolve();
          await releaseGeneration.promise;
          return response("generation answer");
        }
        return response(requests.length === 1 ? "seed answer" : "summary");
      },
    } as never,
  );
  const wired = session as unknown as {
    models: never;
    registry: { install(extension: unknown): void };
    modelTaskContext: ModelTaskContext;
    harness: unknown;
  };
  wired.registry.install({
    name: "acceptance-compaction-gate",
    hooks: [hook(CompactionTask, {
      async beforeCompact() {
        // Automatic compaction and the request are created in the same engine
        // commit; do not depend on which task the scheduler runs first.
        await generationEntered.promise;
        return undefined;
      },
    })],
  });

  let automatic = false;
  const harness = await Harness.open(
    await SqliteStorage.open(sqliteFacade(database), { prefix: "pi_" }),
    {
      models: wired.models,
      registry: wired.registry as never,
      telemetry: wired.modelTaskContext.telemetry,
      settings: () => ({
        retry: { enabled: false },
        compaction: {
          enabled: automatic,
          reserveTokens: 512,
          backgroundTokens: 127_000,
          keepRecentTokens: 1,
        },
      }),
    },
    BACKGROUND_CONTEXT,
  );
  cleanups.push(async () => {
    releaseGeneration.resolve();
    await harness.close(BACKGROUND_CONTEXT);
    database.close();
  });
  const conversation = await harness.root(BACKGROUND_CONTEXT, {
    agent: { model: { provider: defaultModel("default").provider, modelId: "default" } },
  });
  wired.harness = {
    session: () => ({
      id: conversation.id,
      events: () => watchEvents(harness, conversation.id, BACKGROUND_CONTEXT),
    }),
    conversation: async () => conversation,
  };

  const stream = await watchEvents(harness, conversation.id, BACKGROUND_CONTEXT);
  stream.start(async (events) => {
    for (const event of events) {
      if (event.type === "compaction_start") {
        started.resolve({ taskId: Number(event.taskId), reason: event.reason, blocking: event.blocking });
      } else if (event.type === "compaction_end") {
        ended.resolve({ taskId: Number(event.taskId), error: event.error });
      }
    }
  });
  cleanups.push(async () => { await stream.stop(); });

  const seed = await conversation.submit({
    type: "input", requestId: "seed", content: "seed ".repeat(2_000),
    entry: { kind: BOARD_MESSAGE_KIND, data: { messageId: "seed" } },
  }, BACKGROUND_CONTEXT);
  await within(seed.wait(BACKGROUND_CONTEXT));
  expect(requests).toHaveLength(1);

  automatic = true;
  const operation = await conversation.submit({
    type: "input", requestId: "overlap", content: "Continue the conversation.",
    entry: { kind: BOARD_MESSAGE_KIND, data: { messageId: "overlap" } },
  }, BACKGROUND_CONTEXT);
  await within(generationEntered.promise);
  const start = await within(started.promise);
  const end = await within(ended.promise);
  expect(start.reason).toBe("threshold");
  expect(start.blocking).toBe(false);
  expect(end.taskId).toBe(start.taskId);
  expect(compactionReads).toEqual([start.taskId]);
  const generation = requests[1]!;
  expect(generation.task).not.toBe(String(start.taskId));
  if (failCompactionRead) {
    expect(end.error).toBeDefined();
    expect(requests).toHaveLength(2);
  } else {
    expect(end.error).toBeUndefined();
    expect(requests).toHaveLength(3);
    expect(requests[2]!.task).toBe(String(start.taskId));
    expect(requests[2]!.conversation).toBe(generation.conversation);
    expect(requests[2]).not.toEqual(generation);
  }
  releaseGeneration.resolve();
  await within(operation.wait(BACKGROUND_CONTEXT));
}

describe("automatic compaction funding through a hosted session", () => {
  it("funds an overlapping summary under its actual durable task, not the generation", async () => {
    await exercise(false);
  });

  it("does not dispatch the summary when its durable identity is unavailable", async () => {
    await exercise(true);
  });
});
