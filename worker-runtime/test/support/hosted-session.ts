/**
 * A real `HostedSession` on the real engine and in-memory SQLite, with a
 * scripted model. Shared by the worker-runtime tests and the opt-in evals
 * (`evals/`), so an eval replays exactly the request the runtime would send.
 * Only the model transport and the Workers module are stand-ins.
 */
import { Database } from "bun:sqlite";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import type { SqliteDatabase, SqliteExecutor, SqliteValue } from "@earendil-works/chord/sqlite";
import { Harness } from "@earendil-works/pi-durable";
import { SqliteStorage } from "@earendil-works/pi-durable/storage/sqlite";
import { BOARD_MESSAGE_KIND, type GenerationIdentity, type ThreadEntry } from "../../src/contract.ts";
import { defaultModel } from "../../src/model.ts";
import type { ModelTaskContext } from "../../src/model-task-context.ts";
import { ensureDiagnosticResetIndex } from "../../src/session-summary.ts";

// The Workers module is absent in Bun; install its lifecycle stub before
// loading the real session. The engine and SQLite are not stubbed. A runtime
// plugin works both under `bun test` and under `bun run` (the evals).
Bun.plugin({
  name: "cloudflare-workers-stub",
  setup(build) {
    build.module("cloudflare:workers", () => ({
      loader: "object",
      exports: {
        DurableObject: class {
          constructor(readonly ctx: unknown, readonly env: unknown) {}
        },
      },
    }));
  },
});

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

export const chunk = (delta: Record<string, unknown>, finish: string | null = null, model = "synthetic-model") =>
  `data: ${JSON.stringify({
    id: "synthetic-response", object: "chat.completion.chunk", created: 1,
    model, choices: [{ index: 0, delta, finish_reason: finish }],
  })}\n\n`;
export function answer(text: string) {
  return new Response(
    chunk({ role: "assistant", content: "" }) + chunk({ content: text }) +
      chunk({}, "stop") + "data: [DONE]\n\n",
    { headers: { "content-type": "text/event-stream" } },
  );
}

/** The engine's summarization request opens with its own system prompt. */
export const isSummary = (input: Record<string, unknown>) => {
  const first = (input.messages as Array<{ role?: string; content?: unknown }>)[0];
  return first?.role === "system" && JSON.stringify(first.content).includes("context summarization assistant");
};

/**
 * Compaction settings are read on every engine call, so a caller may change
 * `reserveTokens` between turns to force a compaction at a chosen point.
 * `tools` replaces the artifact tools below with the caller's own.
 */
export type SessionOptions = {
  keepRecentTokens?: number;
  artifact?: string;
  reserveTokens?: number;
  put?: boolean;
  backgroundTokens?: number;
  projectThread?: (entries: readonly ThreadEntry[]) => readonly ThreadEntry[];
  refuseDiagnosticsIndex?: boolean;
  tools?: readonly unknown[];
};

export type RequestRecord = { input: Record<string, unknown>; identity: GenerationIdentity; bytes: number };
export const cleanups: Array<() => Promise<void>> = [];
export async function session(
  respond: (requests: readonly RequestRecord[]) => Response | Promise<Response>,
  options: SessionOptions = {},
) {
  const { HostedSession } = await import("../../src/hosted-session.ts");
  const database = new Database(":memory:");
  const sql = {
    exec(query: string, ...bindings: (string | number | null)[]) {
      if (options.refuseDiagnosticsIndex && query.startsWith("CREATE INDEX IF NOT EXISTS cadence_diagnostic_reset_heads"))
        throw new Error("SQLITE_FULL");
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
      tools: options.tools ?? (options.artifact === undefined && !options.put ? [] : [{
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
      }]),
      projectThread: options.projectThread,
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
  ensureDiagnosticResetIndex(sql);
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
    async compact() {
      const id = await conversation.compact(undefined, BACKGROUND_CONTEXT);
      return (await harness.waitForTask(id, BACKGROUND_CONTEXT)).state.outcome;
    },
    async writeRecord(entry: Record<string, unknown>) {
      const operation = await conversation.submit({ type: "write", entry } as never, BACKGROUND_CONTEXT);
      await operation.wait(BACKGROUND_CONTEXT);
    },
    inspectReconstructed: (readSql: typeof sql = sql) => (Object.assign(Object.create(HostedSession.prototype), {
      ctx: { id, storage: { sql: readSql } },
    }) as InstanceType<typeof HostedSession>).sessionSummary(),
    unavailableTranscript: (sessionId: string) => (Object.assign(Object.create(HostedSession.prototype), {
      ctx: { id, storage: { sql: { exec() { throw new Error("SQL unavailable"); } } } },
      options: {},
    }) as InstanceType<typeof HostedSession>).sessionTranscript(sessionId, 0),
    async submit(content: string, transcriptRole: "operator" | null = "operator") {
      const id = `operation-${++operations}`;
      return conversation.submit({
        type: "input", requestId: id, content,
        entry: { kind: BOARD_MESSAGE_KIND, data: { messageId: id, ...(transcriptRole && { transcriptRole }) } },
      }, BACKGROUND_CONTEXT);
    },
  };
}
