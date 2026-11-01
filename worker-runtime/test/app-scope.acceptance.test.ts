import { Database } from "bun:sqlite";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, mock } from "bun:test";
import type {
  Api,
  AssistantMessage,
  Message,
  Model,
  Provider,
} from "@earendil-works/pi-ai";
import { createModels } from "@earendil-works/pi-ai/models";
import { createAssistantMessageEventStream } from "@earendil-works/pi-ai/utils/event-stream";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import type {
  SqliteDatabase,
  SqliteExecutor,
  SqliteValue,
} from "@earendil-works/chord/sqlite";
import {
  createRegistry,
  Harness,
  LiveDoc,
  MemoryStorage,
} from "@earendil-works/pi-durable";
import { SqliteStorage } from "@earendil-works/pi-durable/storage/sqlite";
import {
  BOARD_MESSAGE_KIND,
  type AppScope,
  type RuntimeTool,
} from "../src/contract.ts";
import { runtimeExtension, type AppScopeResolver } from "../src/engine.ts";
import { readSubmissionEntry } from "../src/identity.ts";
import {
  guardOperationRequests,
  operationContextExtension,
  runAppScope,
} from "../src/operation-context.ts";
import {
  OperationScopes,
  parseEntryAppScope,
  type OperationEnvelope,
  type OperationScopeSql,
} from "../src/operation-scope.ts";

/**
 * CAD-1953: one operation id keeps the full envelope it was admitted with —
 * the text and the host-verified app scope — and the runtime never adopts
 * engine work it did not admit itself.
 *
 * The engine deduplicates by request id alone: a repeated id returns its
 * existing submission after comparing the submission *type*, never its text or
 * metadata. The runtime therefore owns a durable envelope guard: the first
 * admission of an operation id records its text and app scope, an identical
 * replay is the same operation, and a changed text or scope is refused before
 * any engine write. A session-wide mutable "current scope" would race queued
 * turns, so the guard is keyed per operation id.
 *
 * Two upgrade edges are part of the contract:
 * - A submission made before the guard existed has engine state but no guard
 *   row. A later turn with the same operation id must not be accepted as a new
 *   scoped replay of that legacy work, queued or settled.
 * - A stored record that can no longer be read as the envelope it was admitted
 *   with fails closed instead of being matched.
 *
 * What this file proves: the real `HostedSession.submit`/`ask` call the guard
 * before the engine and refuse these cases; the guard's durable semantics on
 * real SQLite; and the real engine's queue/replay behaviour, both on
 * `MemoryStorage` and on the session's own SQLite where the guard reads the
 * engine's `pi_` tables.
 *
 * What this file does NOT prove: the Durable Object construction and lifecycle
 * around `HostedSession`. This repository has no Workers runtime rig — no
 * `wrangler`/`miniflare`/`@cloudflare/vitest-pool-workers` configuration, and
 * `bun test` cannot resolve `cloudflare:workers` — so the platform base is
 * stubbed here and the session is driven through its prototype methods. That
 * gap is reported as a blocker, not papered over.
 */

// `hosted-session.ts` imports its Durable Object base from the platform
// module. `bun test` cannot resolve it, so the base is stubbed for this file
// only; no session is constructed, and the stub is never reached at runtime.
mock.module("cloudflare:workers", () => ({
  DurableObject: class {
    constructor(_ctx: unknown, _env: unknown) {}
  },
}));

const MODEL: Model<Api> = {
  id: "scripted",
  name: "Scripted",
  api: "openai-completions",
  provider: "scripted",
  baseUrl: "https://scripted.invalid",
  input: ["text"],
  reasoning: false,
  contextWindow: 128_000,
  maxTokens: 8_192,
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
};

const USAGE: AssistantMessage["usage"] = {
  input: 0,
  output: 0,
  cacheRead: 0,
  cacheWrite: 0,
  totalTokens: 0,
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
};

/** An install-scoped binding: the app context is optional. */
const LEDGER: AppScope = { installId: "install_ledger" };

/** The same install bound to one of its contexts. */
const LEDGER_CAMPAIGN: AppScope = {
  installId: "install_ledger",
  contextId: "campaign",
};

const SOCIAL: AppScope = {
  installId: "install_social_content",
  contextId: "default",
};

function envelope(
  operationId: string,
  text: string,
  app?: AppScope,
): OperationEnvelope {
  return app === undefined ? { operationId, text } : { operationId, text, app };
}

function assistantReply(text: string): AssistantMessage {
  return {
    role: "assistant",
    content: [{ type: "text", text }],
    api: "openai-completions",
    provider: "scripted",
    model: "scripted",
    usage: USAGE,
    stopReason: "stop",
    timestamp: Date.now(),
  };
}

function toolCallReply(id: string, name: string): AssistantMessage {
  return {
    ...assistantReply(""),
    content: [{ type: "toolCall", id, name, arguments: {} }],
    stopReason: "toolUse",
  };
}

function messageText(message: Message): string {
  if (typeof message.content === "string") return message.content;
  return message.content
    .flatMap((part) => (part.type === "text" ? [part.text] : []))
    .join("");
}

function lastUserText(messages: readonly Message[]): string {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index]!;
    if (message.role === "user") return messageText(message);
  }
  return "";
}

/** What one provider request saw: the composed system context and the turn. */
type ScopeObservation = {
  readonly system: string;
  readonly user: string;
  readonly hasToolResult: boolean;
};

function systemTextOf(messages: readonly Message[]): string {
  return messages
    .filter((message) => message.role === "system")
    .map(messageText)
    .join("\n");
}

/**
 * A provider-shaped, offline `Models` instance: what the harness's real
 * generation task calls through `Models.streamSimple`. By default the first
 * request opens a gate, so a test can submit another turn while the first one
 * is running and observe pi's queue instead of racing it; `gate: false` replies
 * at once. Every request's system context and user turn are recorded, so a test
 * reads the context the runtime actually injected.
 */
function scriptedModels(
  reply: string | ((observation: ScopeObservation) => AssistantMessage),
  options: { gate?: boolean } = {},
) {
  const started = Promise.withResolvers<void>();
  const blocked = Promise.withResolvers<void>();
  const observations: ScopeObservation[] = [];
  const requests: string[] = [];
  const respond = (context: { messages: readonly Message[] }) => {
    const observation: ScopeObservation = {
      system: systemTextOf(context.messages),
      user: lastUserText(context.messages),
      hasToolResult: context.messages.some(
        (message) => message.role === "toolResult",
      ),
    };
    observations.push(observation);
    requests.push(observation.user);
    started.resolve();
    const stream = createAssistantMessageEventStream();
    const message =
      typeof reply === "string" ? assistantReply(reply) : reply(observation);
    if (options.gate === false)
      queueMicrotask(() => stream.end(message));
    else void blocked.promise.then(() => stream.end(message));
    return stream;
  };
  const provider: Provider = {
    id: "scripted",
    name: "Scripted",
    auth: {
      apiKey: {
        name: "Scripted",
        resolve: async () => ({ auth: { apiKey: "scripted" } }),
      },
    },
    getModels: () => [MODEL],
    stream: (_model, context) => respond(context),
    streamSimple: (_model, context) => respond(context),
  };
  const models = createModels();
  models.setProvider(provider);
  return {
    models,
    observations,
    requests,
    started: started.promise,
    release: () => blocked.resolve(),
  };
}

/** `node:sqlite`-shaped adapter over Bun's real SQLite for these tests only. */
function sqlStore(database: Database): OperationScopeSql {
  return {
    exec<T = Record<string, unknown>>(
      query: string,
      ...bindings: (string | number | null)[]
    ) {
      if (/^\s*(SELECT|PRAGMA|WITH)/i.test(query)) {
        const rows = database.prepare(query).all(...bindings) as unknown as T[];
        return { toArray: () => rows };
      }
      database.prepare(query).run(...bindings);
      return { toArray: () => [] as T[] };
    },
  };
}

/**
 * The engine's portable SQLite facade over the same Bun database, so the
 * engine's own `pi_` tables and the guard's table live in one session SQLite —
 * exactly the layout a Durable Object has.
 */
function sqliteFacade(database: Database): SqliteDatabase {
  const executor: SqliteExecutor = {
    async exec(sql: string) {
      database.exec(sql);
    },
    async run(sql: string, ...params: SqliteValue[]) {
      database.prepare(sql).run(...(params as never[]));
    },
    async get<T extends object>(sql: string, ...params: SqliteValue[]) {
      // The portable facade answers `undefined` for no row; Bun's `get`
      // answers `null`. The engine branches on `undefined`, so translate.
      const row = database.prepare(sql).get(...(params as never[])) as
        | T
        | null
        | undefined;
      return row ?? undefined;
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
    // The test owns the Bun connection and closes it itself.
    async close() {},
  };
}

const directories: string[] = [];
afterEach(() => {
  for (const directory of directories)
    rmSync(directory, { recursive: true, force: true });
  directories.length = 0;
});

function sessionDatabase(): { database: Database; path: string } {
  const directory = mkdtempSync(join(tmpdir(), "cadence-app-scope-"));
  directories.push(directory);
  const path = join(directory, "session.sqlite");
  return { database: new Database(path), path };
}

type GuardRow = {
  readonly table: string;
  readonly row: Record<string, unknown>;
};

/**
 * The guard's own tables, discovered rather than assumed: everything the store
 * created that is not the engine's `pi_` schema. This acceptance checks
 * behaviour and durability, not a column layout, so a digest or any other
 * storage change keeps the fixture valid.
 */
function guardTables(database: Database): string[] {
  const rows = database
    .prepare(
      `SELECT name FROM sqlite_master
        WHERE type='table'
          AND name NOT LIKE 'sqlite_%'
          AND name NOT LIKE 'pi_%'
        ORDER BY name`,
    )
    .all() as Array<{ name: string }>;
  if (rows.length === 0)
    throw new Error("the guard created no table");
  return rows.map((row) => row.name);
}

/** Every stored guard record, whatever its layout. */
function guardRows(database: Database): GuardRow[] {
  return guardTables(database).flatMap((table) =>
    (
      database.prepare(`SELECT * FROM ${table}`).all() as Array<
        Record<string, unknown>
      >
    ).map((row) => ({ table, row })),
  );
}

/**
 * Make the stored record unreadable without assuming its layout: keep the key
 * and blank every other column. A stored text, digest or scope is then
 * unparsable or matches nothing, which the guard must refuse.
 */
function corruptStoredRecord(database: Database): void {
  for (const table of guardTables(database)) {
    const rows = database.prepare(`SELECT * FROM ${table}`).all() as Array<
      Record<string, unknown>
    >;
    if (rows.length === 0) continue;
    const columns = database
      .prepare(`PRAGMA table_info(${table})`)
      .all() as Array<{ name: string; type: string; pk: number }>;
    const key = (columns.find((column) => column.pk === 1) ?? columns[0])?.name;
    if (key === undefined)
      throw new Error("the guard table has no key column");
    const rest = columns.filter((column) => column.name !== key);
    if (rest.length === 0)
      throw new Error("the guard table stores nothing to corrupt");
    const update = database.prepare(
      `UPDATE ${table} SET ${rest
        .map((column) => `${column.name}=?`)
        .join(",")} WHERE ${key}=?`,
    );
    for (const row of rows)
      update.run(
        ...rest.map((column) => (/INT/i.test(column.type) ? 0 : "")),
        row[key] as string | number | null,
      );
    return;
  }
  throw new Error("the guard stored no record to corrupt");
}

/**
 * The real `HostedSession` prototype over a stubbed engine. The platform base
 * is stubbed above because `bun test` cannot load `cloudflare:workers`; the
 * methods under test are the runtime's own, and `operationScopes` is the guard
 * the session holds.
 */
async function sessionOver(
  engine: {
    submit(
      text: string,
      options: { operationId: string },
    ): Promise<{ accepted: boolean; operationId: string }>;
    prompt(
      text: string,
      options: { operationId: string },
    ): Promise<{ status: "done"; text: string; operationId: string }>;
    pending?(): Promise<
      readonly {
        operationId: string;
        session: string;
        status: "queued" | "running" | "parked";
      }[]
    >;
  },
  operationScopes: OperationScopes,
) {
  // Static import would be hoisted above the `cloudflare:workers` mock and
  // fail to resolve; the platform module does not exist under `bun test`.
  const { HostedSession } = await import("../src/hosted-session.ts");
  return Object.assign(Object.create(HostedSession.prototype), {
    options: {},
    operationScopes,
    // The engine stub always answers `pending`; a case that models pre-upgrade
    // queued work supplies its own.
    harness: { pending: async () => [], ...engine },
  }) as InstanceType<typeof HostedSession>;
}

describe("HostedSession admission", () => {
  it("refuses a changed app scope or text before the engine sees it", async () => {
    const { database } = sessionDatabase();
    const scopes = new OperationScopes(sqlStore(database));
    const submissions: Array<{ text: string; operationId: string }> = [];
    const accepted = new Set<string>();
    const session = await sessionOver(
      {
        submit: async (text, options) => {
          submissions.push({ text, operationId: options.operationId });
          const first = !accepted.has(options.operationId);
          accepted.add(options.operationId);
          return { accepted: first, operationId: options.operationId };
        },
        prompt: async () => {
          throw new Error("ask is not part of this case");
        },
      },
      scopes,
    );
    try {
      expect(
        await session.submit("reconcile invoices", "op-ledger", LEDGER),
      ).toBe(true);

      await expect(
        session.submit("reconcile invoices", "op-ledger", SOCIAL),
      ).rejects.toThrow();
      await expect(
        session.submit("pay the invoices", "op-ledger", LEDGER),
      ).rejects.toThrow();
      await expect(
        session.submit("reconcile invoices", "op-ledger"),
      ).rejects.toThrow();

      // No refusal reached the engine.
      expect(submissions).toEqual([
        { text: "reconcile invoices", operationId: "op-ledger" },
      ]);

      // An identical envelope is the same operation: the guard allows it and
      // the engine replays instead of accepting new work.
      expect(
        await session.submit("reconcile invoices", "op-ledger", LEDGER),
      ).toBe(false);

      // Home positive control: text alone, no app scope, replayed the same.
      expect(await session.submit("what is on today", "op-home")).toBe(true);
      expect(await session.submit("what is on today", "op-home")).toBe(false);
    } finally {
      database.close();
    }
  });

  it("refuses a changed envelope on ask before the engine runs", async () => {
    const { database } = sessionDatabase();
    const scopes = new OperationScopes(sqlStore(database));
    const prompts: string[] = [];
    const session = await sessionOver(
      {
        submit: async () => {
          throw new Error("submit is not part of this case");
        },
        prompt: async (text, options) => {
          prompts.push(text);
          return {
            status: "done",
            text: "acknowledged",
            operationId: options.operationId,
          };
        },
      },
      scopes,
    );
    try {
      expect(
        (await session.ask("reconcile invoices", "op-ledger", LEDGER)).status,
      ).toBe("done");
      await expect(
        session.ask("reconcile invoices", "op-ledger", SOCIAL),
      ).rejects.toThrow();
      await expect(
        session.ask("pay the invoices", "op-ledger", LEDGER),
      ).rejects.toThrow();
      expect(prompts).toEqual(["reconcile invoices"]);
    } finally {
      database.close();
    }
  });

  it("refuses engine work the guard never admitted, queued or settled", async () => {
    const { database } = sessionDatabase();
    const scripted = scriptedModels("acknowledged");
    const storage = await SqliteStorage.open(sqliteFacade(database), {
      prefix: "pi_",
    });
    const harness = await Harness.open(
      storage,
      { models: scripted.models, registry: createRegistry() },
      BACKGROUND_CONTEXT,
    );
    try {
      // Pre-upgrade state: the engine stores these submissions in the session
      // SQLite, and the guard table does not exist yet.
      const conversation = await harness.root(BACKGROUND_CONTEXT, {
        agent: { model: { provider: "scripted", modelId: "scripted" } },
      });
      const running = await conversation.submit(
        { type: "input", requestId: "op-running", content: "reconcile invoices" },
        BACKGROUND_CONTEXT,
      );
      await scripted.started;
      const queued = await conversation.submit(
        {
          type: "input",
          requestId: "op-legacy-queued",
          content: "reconcile invoices",
        },
        BACKGROUND_CONTEXT,
      );
      scripted.release();
      expect((await running.wait(BACKGROUND_CONTEXT)).status).toBe("done");
      expect((await queued.wait(BACKGROUND_CONTEXT)).status).toBe("done");
      const settled = await conversation.submit(
        {
          type: "input",
          requestId: "op-legacy-settled",
          content: "reconcile invoices",
        },
        BACKGROUND_CONTEXT,
      );
      expect((await settled.wait(BACKGROUND_CONTEXT)).status).toBe("done");

      // Only now does the guard exist, over the same SQLite the engine wrote.
      const scopes = new OperationScopes(sqlStore(database));
      const calls: string[] = [];
      const session = await sessionOver(
        {
          submit: async (text, options) => {
            calls.push(options.operationId);
            return { accepted: true, operationId: options.operationId };
          },
          prompt: async () => {
            throw new Error("ask is not part of this case");
          },
        },
        scopes,
      );

      // The engine holds both ids; adopting either would replay a turn this
      // guard never admitted for this caller's scope.
      await expect(
        session.submit("reconcile invoices", "op-legacy-queued", LEDGER),
      ).rejects.toThrow();
      await expect(
        session.submit("reconcile invoices", "op-legacy-settled", LEDGER),
      ).rejects.toThrow();
      // A retry is refused again, so no binding was recorded for either.
      await expect(
        session.submit("reconcile invoices", "op-legacy-queued", LEDGER),
      ).rejects.toThrow();
      // Nor can another scope claim them.
      await expect(
        session.submit("reconcile invoices", "op-legacy-queued", SOCIAL),
      ).rejects.toThrow();
      // None of the refusals reached the engine, and none recorded a binding.
      expect(calls).toEqual([]);
      expect(guardRows(database)).toEqual([]);

      // Positive safe control: a new operation is accepted, and its own
      // identical replay is the engine's replay, not a refusal.
      expect(
        await session.submit("reconcile invoices", "op-fresh", LEDGER),
      ).toBe(true);
      expect(calls).toEqual(["op-fresh"]);
    } finally {
      scripted.release();
      await harness.close(BACKGROUND_CONTEXT);
      database.close();
    }
  });
});

/**
 * The session's two scope readers, over the engine's own SQLite: the model
 * hook's loader (`HostedSession.operationAppScope`) and the tool resolver
 * (`HostedSession.toolAppScope`). Both read the placed entry the engine stored,
 * never a caller, so a test observes the scope the runtime actually admitted.
 */
function scopeReaders(database: Database) {
  const store = sqlStore(database);
  const load = (root: string): AppScope | null => {
    const entry = readSubmissionEntry(store, root);
    return entry === null ? null : parseEntryAppScope(entry.kind, entry.data);
  };
  const forTool: AppScopeResolver = async (api, context) => {
    const live = await api.snapshot(LiveDoc, api.conversationId, context);
    const run = live?.run;
    if (run === undefined || run.inputs.length === 0) return null;
    return runAppScope(run.inputs, load);
  };
  return { load, forTool };
}

/** One operator input exactly as the runtime places it: message id plus scope. */
function operationEntry(operationId: string, app?: AppScope) {
  return {
    kind: BOARD_MESSAGE_KIND,
    data: app === undefined ? { messageId: operationId } : { messageId: operationId, app },
  };
}

describe("operation scope propagation", () => {
  it("gives the model and the tool the turn's admitted scope, and Home none", async () => {
    const { database } = sessionDatabase();
    const { load, forTool } = scopeReaders(database);
    const seen: Array<AppScope | undefined> = [];
    const probe: RuntimeTool = {
      name: "scope_probe",
      description: "Report the app scope of the turn.",
      parameters: { type: "object", properties: {} },
      async execute(_input, invocation) {
        seen.push(invocation.app);
        return { output: [{ type: "text", text: "noted" }] };
      },
    };
    let calls = 0;
    const scripted = scriptedModels(
      ({ hasToolResult }) =>
        hasToolResult
          ? assistantReply("done")
          : toolCallReply(`scope-call-${(calls += 1)}`, "scope_probe"),
      { gate: false },
    );
    const registry = createRegistry();
    // Production wiring: the session authors the attempt and installs the
    // model-boundary guard on the same Models the engine calls.
    const operationMessages = new WeakSet<object>();
    registry.install(operationContextExtension(load, operationMessages));
    guardOperationRequests(scripted.models, operationMessages);
    registry.install(
      runtimeExtension({ name: "scope-tools", tools: [probe] }, undefined, forTool),
    );
    const storage = await SqliteStorage.open(sqliteFacade(database), {
      prefix: "pi_",
    });
    const harness = await Harness.open(
      storage,
      { models: scripted.models, registry },
      BACKGROUND_CONTEXT,
    );
    try {
      const conversation = await harness.root(BACKGROUND_CONTEXT, {
        agent: { model: { provider: "scripted", modelId: "scripted" } },
      });
      const ledger = await conversation.submit(
        {
          type: "input",
          requestId: "op-ledger",
          content: "ledger question",
          entry: operationEntry("op-ledger", {
            installId: "install_ledger",
            contextId: "campaign",
          }),
        },
        BACKGROUND_CONTEXT,
      );
      expect((await ledger.wait(BACKGROUND_CONTEXT)).status).toBe("done");

      const home = await conversation.submit(
        {
          type: "input",
          requestId: "op-home",
          content: "home question",
          entry: operationEntry("op-home"),
        },
        BACKGROUND_CONTEXT,
      );
      expect((await home.wait(BACKGROUND_CONTEXT)).status).toBe("done");

      const ledgerRequest = scripted.observations.find(
        (observation) => observation.user === "ledger question",
      );
      expect(ledgerRequest, "the app turn reached the provider").toBeDefined();
      expect(ledgerRequest?.system).toContain(
        "install_id install_ledger, context_id campaign",
      );
      // The statement conveys the binding, never authority.
      expect(ledgerRequest?.system).toContain("conveys no tool authority");

      const homeRequest = scripted.observations.find(
        (observation) => observation.user === "home question",
      );
      // Home is not refused: it reaches the provider without a scope statement.
      expect(homeRequest, "the Home turn reached the provider").toBeDefined();
      expect(homeRequest?.system).not.toContain("install_id install_ledger");
      expect(homeRequest?.system).not.toContain("conveys no tool authority");

      // The tool call of each turn saw that turn's scope; Home saw none.
      expect(seen).toEqual([
        { installId: "install_ledger", contextId: "campaign" },
        undefined,
      ]);
    } finally {
      await harness.close(BACKGROUND_CONTEXT);
      database.close();
    }
  });

  it(
    "refuses a run that mixes two app scopes, and keeps one-scope batches attributable",
    async () => {
      const { database } = sessionDatabase();
      const { load, forTool } = scopeReaders(database);
      const toolStarted = Promise.withResolvers<void>();
      const toolGate = Promise.withResolvers<void>();
      const slow: RuntimeTool = {
        name: "slow_probe",
        description: "Hold the run open while another turn steers into it.",
        parameters: { type: "object", properties: {} },
        async execute() {
          toolStarted.resolve();
          await toolGate.promise;
          return { output: [{ type: "text", text: "noted" }] };
        },
      };
      const scripted = scriptedModels(
        ({ hasToolResult, user }) =>
          hasToolResult || user !== "mixed question"
            ? assistantReply("done")
            : toolCallReply("mixed-call", "slow_probe"),
        { gate: false },
      );
      const registry = createRegistry();
      const operationMessages = new WeakSet<object>();
      registry.install(operationContextExtension(load, operationMessages));
      guardOperationRequests(scripted.models, operationMessages);
      registry.install(
        runtimeExtension({ name: "slow-tools", tools: [slow] }, undefined, forTool),
      );
      const storage = await SqliteStorage.open(sqliteFacade(database), {
        prefix: "pi_",
      });
      const harness = await Harness.open(
        storage,
        { models: scripted.models, registry },
        BACKGROUND_CONTEXT,
      );
      try {
        const conversation = await harness.root(BACKGROUND_CONTEXT, {
          agent: { model: { provider: "scripted", modelId: "scripted" } },
        });

        // Two settled operations with different scopes, and a Home one: the
        // helper reads the engine's own placed entries.
        const ledger = await conversation.submit(
          {
            type: "input",
            requestId: "op-ledger",
            content: "first",
            entry: operationEntry("op-ledger", LEDGER),
          },
          BACKGROUND_CONTEXT,
        );
        expect((await ledger.wait(BACKGROUND_CONTEXT)).status).toBe("done");
        const social = await conversation.submit(
          {
            type: "input",
            requestId: "op-social",
            content: "second",
            entry: operationEntry("op-social", SOCIAL),
          },
          BACKGROUND_CONTEXT,
        );
        expect((await social.wait(BACKGROUND_CONTEXT)).status).toBe("done");
        const home = await conversation.submit(
          {
            type: "input",
            requestId: "op-home",
            content: "third",
            entry: operationEntry("op-home"),
          },
          BACKGROUND_CONTEXT,
        );
        expect((await home.wait(BACKGROUND_CONTEXT)).status).toBe("done");

        expect(runAppScope([ledger.id], load)).toEqual(LEDGER);
        expect(runAppScope([social.id], load)).toEqual(SOCIAL);
        expect(runAppScope([home.id], load)).toBeNull();
        // One scope repeated is one batch; two different scopes are refused.
        expect(runAppScope([ledger.id, ledger.id], load)).toEqual(LEDGER);
        expect(() =>
          runAppScope([ledger.id, social.id], load),
        ).toThrow(/different app scopes/);
        expect(() => runAppScope([ledger.id, home.id], load)).toThrow(
          /different app scopes/,
        );

        // A live run that a steer makes mixed is refused by the real
        // extension. Its `beforeRequest` hook is only *reported* by the engine
        // when it throws, so the extension marks the attempt and the refusal
        // happens at the model boundary; this asserts the boundary, not an
        // assumed propagation.
        const before = scripted.observations.length;
        const running = await conversation.submit(
          {
            type: "input",
            requestId: "op-mixed-ledger",
            content: "mixed question",
            entry: operationEntry("op-mixed-ledger", LEDGER),
          },
          BACKGROUND_CONTEXT,
        );
        await toolStarted.promise;
        const steer = await conversation.submit(
          {
            type: "input",
            requestId: "op-mixed-social",
            content: "social question",
            whenBusy: "steer",
            entry: operationEntry("op-mixed-social", SOCIAL),
          },
          BACKGROUND_CONTEXT,
        );
        toolGate.resolve();
        await harness.waitForIdle(BACKGROUND_CONTEXT);

        // The tool round ran, and the mixed run never reached the provider a
        // second time: the model boundary refused the unattributable attempt.
        expect(scripted.observations.length).toBe(before + 1);
        // No request carrying the steered turn's scope escaped.
        expect(
          scripted.observations.filter(
            (observation) => observation.user === "social question",
          ),
        ).toHaveLength(0);
        const runningSettled = await running.status(BACKGROUND_CONTEXT);
        expect(runningSettled.status).toBe("unanswered");
        if (runningSettled.status === "unanswered")
          expect(runningSettled.reason).toBeTruthy();
        const steerSettled = await steer.status(BACKGROUND_CONTEXT);
        expect(steerSettled.status).toBe("unanswered");
        if (steerSettled.status === "unanswered")
          expect(steerSettled.reason).toBeTruthy();
      } finally {
        toolGate.resolve();
        await harness.close(BACKGROUND_CONTEXT);
        database.close();
      }
    },
    30_000,
  );

  it(
    "refuses an unattested attempt when the operation hook is missing, including Home",
    async () => {
      const { database } = sessionDatabase();
      const scripted = scriptedModels("must not run", { gate: false });
      // Production wiring, but the extension that attests an attempt is not
      // installed: the model boundary must fail closed on its own.
      const authored = new WeakSet<object>();
      guardOperationRequests(scripted.models, authored);
      const storage = await SqliteStorage.open(sqliteFacade(database), {
        prefix: "pi_",
      });
      const harness = await Harness.open(
        storage,
        { models: scripted.models, registry: createRegistry() },
        BACKGROUND_CONTEXT,
      );
      try {
        const conversation = await harness.root(BACKGROUND_CONTEXT, {
          agent: { model: { provider: "scripted", modelId: "scripted" } },
        });

        const appTurn = await conversation.submit(
          {
            type: "input",
            requestId: "op-app",
            content: "ledger question",
            entry: operationEntry("op-app", LEDGER),
          },
          BACKGROUND_CONTEXT,
        );
        await harness.waitForIdle(BACKGROUND_CONTEXT);
        expect(scripted.observations).toHaveLength(0);
        expect((await appTurn.status(BACKGROUND_CONTEXT)).status).toBe(
          "unanswered",
        );

        // Home is not an exemption: no attestation means no provider call.
        const homeTurn = await conversation.submit(
          {
            type: "input",
            requestId: "op-home",
            content: "home question",
            entry: operationEntry("op-home"),
          },
          BACKGROUND_CONTEXT,
        );
        await harness.waitForIdle(BACKGROUND_CONTEXT);
        expect(scripted.observations).toHaveLength(0);
        expect((await homeTurn.status(BACKGROUND_CONTEXT)).status).toBe(
          "unanswered",
        );
      } finally {
        await harness.close(BACKGROUND_CONTEXT);
        database.close();
      }
    },
    30_000,
  );

  it(
    "refuses an unattested attempt when the operation hook throws, including Home",
    async () => {
      const { database } = sessionDatabase();
      const scripted = scriptedModels("must not run", { gate: false });
      const authored = new WeakSet<object>();
      guardOperationRequests(scripted.models, authored);
      const registry = createRegistry();
      registry.install(
        operationContextExtension(() => {
          throw new Error("the host's operation scope is unavailable");
        }, authored),
      );
      const storage = await SqliteStorage.open(sqliteFacade(database), {
        prefix: "pi_",
      });
      const harness = await Harness.open(
        storage,
        { models: scripted.models, registry },
        BACKGROUND_CONTEXT,
      );
      try {
        const conversation = await harness.root(BACKGROUND_CONTEXT, {
          agent: { model: { provider: "scripted", modelId: "scripted" } },
        });

        // The engine only reports a hook throw and continues the attempt, so
        // the refusal must come from the missing attestation at the boundary.
        const appTurn = await conversation.submit(
          {
            type: "input",
            requestId: "op-app",
            content: "ledger question",
            entry: operationEntry("op-app", LEDGER),
          },
          BACKGROUND_CONTEXT,
        );
        await harness.waitForIdle(BACKGROUND_CONTEXT);
        expect(scripted.observations).toHaveLength(0);
        expect((await appTurn.status(BACKGROUND_CONTEXT)).status).toBe(
          "unanswered",
        );

        const homeTurn = await conversation.submit(
          {
            type: "input",
            requestId: "op-home",
            content: "home question",
            entry: operationEntry("op-home"),
          },
          BACKGROUND_CONTEXT,
        );
        await harness.waitForIdle(BACKGROUND_CONTEXT);
        expect(scripted.observations).toHaveLength(0);
        expect((await homeTurn.status(BACKGROUND_CONTEXT)).status).toBe(
          "unanswered",
        );
      } finally {
        await harness.close(BACKGROUND_CONTEXT);
        database.close();
      }
    },
    30_000,
  );
});

describe("hosted operation envelope", () => {
  it("binds one operation id to its text and app scope and replays an identical envelope", () => {
    const { database } = sessionDatabase();
    const scopes = new OperationScopes(sqlStore(database));
    try {
      const ledger = envelope("op-ledger", "reconcile invoices", LEDGER);
      expect(scopes.admit(ledger)).toEqual(ledger);
      // A retry sends the same envelope again; it is the same operation.
      expect(scopes.admit(ledger)).toEqual(ledger);

      // An install-scoped turn carries no context and is admitted as such.
      const campaign = envelope(
        "op-campaign",
        "draft the campaign",
        LEDGER_CAMPAIGN,
      );
      expect(scopes.admit(campaign)).toEqual(campaign);

      // Home carries no app scope; its text is still the envelope's.
      const home = envelope("op-home", "what is on today");
      expect(scopes.admit(home)).toEqual(home);
      expect(scopes.admit(home)).toEqual(home);
    } finally {
      database.close();
    }
  });

  it("refuses the same operation id with a changed text or a changed app scope", () => {
    const { database } = sessionDatabase();
    const scopes = new OperationScopes(sqlStore(database));
    try {
      const ledger = envelope("op-ledger", "reconcile invoices", LEDGER);
      scopes.admit(ledger);

      // Changed text under the same id is a different operation.
      expect(() =>
        scopes.admit(envelope("op-ledger", "pay the invoices", LEDGER)),
      ).toThrow();
      // Another install, another context, and a newly bound context are all
      // changes to the same operation's scope.
      expect(() =>
        scopes.admit(envelope("op-ledger", "reconcile invoices", SOCIAL)),
      ).toThrow();
      expect(() =>
        scopes.admit(
          envelope("op-ledger", "reconcile invoices", LEDGER_CAMPAIGN),
        ),
      ).toThrow();
      // Dropping the binding is a change too.
      expect(() =>
        scopes.admit(envelope("op-ledger", "reconcile invoices")),
      ).toThrow();

      // The refusals neither reused nor rewrote the admitted envelope.
      expect(scopes.admit(ledger)).toEqual(ledger);

      // A Home turn cannot be promoted into an app turn, and an app turn
      // cannot be demoted to Home.
      const home = envelope("op-home", "what is on today");
      scopes.admit(home);
      expect(() =>
        scopes.admit(envelope("op-home", "what is on today", LEDGER)),
      ).toThrow();
      expect(() =>
        scopes.admit(envelope("op-home", "what is on tomorrow")),
      ).toThrow();
      expect(scopes.admit(home)).toEqual(home);
    } finally {
      database.close();
    }
  });

  it("fails closed on malformed or unknown envelope metadata", () => {
    const { database } = sessionDatabase();
    const scopes = new OperationScopes(sqlStore(database));
    try {
      const valid = envelope("op-ledger", "reconcile invoices", LEDGER);
      expect(() =>
        scopes.admit(envelope("", "reconcile invoices", LEDGER)),
      ).toThrow();
      expect(() =>
        scopes.admit({ ...valid, operationId: 42 as unknown as string }),
      ).toThrow();
      expect(() =>
        scopes.admit({ ...valid, text: undefined as unknown as string }),
      ).toThrow();
      expect(() =>
        scopes.admit(
          envelope("op-ledger", "reconcile invoices", { installId: "" }),
        ),
      ).toThrow();
      expect(() =>
        scopes.admit(
          envelope("op-ledger", "reconcile invoices", {
            installId: "install_ledger",
            contextId: "",
          }),
        ),
      ).toThrow();
      expect(() =>
        scopes.admit(
          envelope("op-ledger", "reconcile invoices", {
            contextId: "default",
          } as unknown as AppScope),
        ),
      ).toThrow();
      expect(() =>
        scopes.admit(
          envelope(
            "op-ledger",
            "reconcile invoices",
            null as unknown as AppScope,
          ),
        ),
      ).toThrow();
      // A browser-supplied field is not metadata this runtime knows; it must
      // never be carried as if the host had verified it.
      expect(() =>
        scopes.admit(
          envelope("op-ledger", "reconcile invoices", {
            installId: "install_ledger",
            contextId: "default",
            authority: "owner",
          } as unknown as AppScope),
        ),
      ).toThrow();

      // A refused admission reserved nothing: the operation is still free.
      expect(scopes.admit(valid)).toEqual(valid);
    } finally {
      database.close();
    }
  });

  it("fails closed when the stored record is unreadable", () => {
    const { database } = sessionDatabase();
    const scopes = new OperationScopes(sqlStore(database));
    try {
      const ledger = envelope("op-ledger", "reconcile invoices", LEDGER);
      expect(scopes.admit(ledger)).toEqual(ledger);

      // Corrupt the stored record without assuming its layout: keep the key,
      // blank every other column. Whether the guard keeps text, a digest or a
      // scope, the stored binding is then unreadable and matches nothing.
      corruptStoredRecord(database);
      expect(() => scopes.admit(ledger)).toThrow();
      expect(() =>
        scopes.admit(envelope("op-ledger", "reconcile invoices")),
      ).toThrow();
      expect(() =>
        scopes.admit(envelope("op-ledger", "reconcile invoices", SOCIAL)),
      ).toThrow();

      // A refusal never rewrites the unreadable record.
      const snapshot = guardRows(database);
      expect(snapshot).toHaveLength(1);
      expect(() => scopes.admit(ledger)).toThrow();
      expect(guardRows(database)).toEqual(snapshot);
    } finally {
      database.close();
    }
  });

  it("keeps an operation's envelope across a reconstruction of the session store", () => {
    const { database, path } = sessionDatabase();
    const ledger = envelope("op-ledger", "reconcile invoices", LEDGER);
    new OperationScopes(sqlStore(database)).admit(ledger);
    database.close();

    // A rebuilt object re-reads the same SQLite; the envelope is the
    // session's, not the instance's.
    const reopened = new Database(path);
    try {
      const rebuilt = new OperationScopes(sqlStore(reopened));
      expect(() =>
        rebuilt.admit(envelope("op-ledger", "reconcile invoices", SOCIAL)),
      ).toThrow();
      expect(() =>
        rebuilt.admit(envelope("op-ledger", "pay the invoices", LEDGER)),
      ).toThrow();
      expect(rebuilt.admit(ledger)).toEqual(ledger);
    } finally {
      reopened.close();
    }
  });

  it("keeps queued turns' envelopes separate while the engine holds both", async () => {
    const { database } = sessionDatabase();
    const scopes = new OperationScopes(sqlStore(database));
    const scripted = scriptedModels("acknowledged");
    const harness = await Harness.open(
      new MemoryStorage(),
      { models: scripted.models, registry: createRegistry() },
      BACKGROUND_CONTEXT,
    );
    try {
      const conversation = await harness.root(BACKGROUND_CONTEXT, {
        agent: { model: { provider: "scripted", modelId: "scripted" } },
      });
      const ledger = envelope("op-ledger", "ledger question", LEDGER);
      const social = envelope("op-social", "social question", SOCIAL);

      // The runtime admits the envelope before the input reaches the engine.
      // These are the guard's records; the submissions below are the engine's.
      expect(scopes.admit(ledger)).toEqual(ledger);
      const first = await conversation.submit(
        {
          type: "input",
          requestId: ledger.operationId,
          content: ledger.text,
          entry: {
            kind: BOARD_MESSAGE_KIND,
            data: { messageId: ledger.operationId, app: ledger.app },
          },
        },
        BACKGROUND_CONTEXT,
      );

      // The first turn is genuinely running when the second one arrives, so
      // the second is admitted into pi's inbox.
      await scripted.started;
      expect(scopes.admit(social)).toEqual(social);
      const second = await conversation.submit(
        {
          type: "input",
          requestId: social.operationId,
          content: social.text,
          entry: {
            kind: BOARD_MESSAGE_KIND,
            data: { messageId: social.operationId, app: social.app },
          },
        },
        BACKGROUND_CONTEXT,
      );

      const queued = await harness.inspect(BACKGROUND_CONTEXT);
      const firstRecord = queued.submissions.find(
        (submission) => submission.requestId === ledger.operationId,
      );
      const secondRecord = queued.submissions.find(
        (submission) => submission.requestId === social.operationId,
      );
      expect(firstRecord?.status).toBe("placed");
      expect(secondRecord?.status).toBe("queued");
      expect(
        queued.submissions.map((submission) => submission.requestId),
      ).toEqual([ledger.operationId, social.operationId]);

      // Neither the running turn nor the queued one can have its scope or its
      // text rewritten by a later caller.
      expect(() =>
        scopes.admit(envelope("op-ledger", "ledger question", SOCIAL)),
      ).toThrow();
      expect(() =>
        scopes.admit(envelope("op-ledger", "another question", LEDGER)),
      ).toThrow();
      expect(() =>
        scopes.admit(envelope("op-social", "social question", LEDGER)),
      ).toThrow();
      expect(() =>
        scopes.admit(envelope("op-social", "another question", SOCIAL)),
      ).toThrow();
      expect(() =>
        scopes.admit(envelope("op-social", "social question")),
      ).toThrow();

      // The refused envelopes added no engine work and disturbed no queued
      // turn; the engine still holds exactly the two admitted operations.
      const after = await harness.inspect(BACKGROUND_CONTEXT);
      expect(
        after.submissions.map((submission) => submission.requestId),
      ).toEqual([ledger.operationId, social.operationId]);

      // An identical envelope is the same operation: the engine replays its
      // existing submission instead of admitting a second one.
      expect(scopes.admit(ledger)).toEqual(ledger);
      const replay = await conversation.submit(
        {
          type: "input",
          requestId: ledger.operationId,
          content: ledger.text,
          entry: {
            kind: BOARD_MESSAGE_KIND,
            data: { messageId: ledger.operationId, app: ledger.app },
          },
        },
        BACKGROUND_CONTEXT,
      );
      expect(replay.id).toBe(first.id);
      expect(
        (await harness.inspect(BACKGROUND_CONTEXT)).submissions,
      ).toHaveLength(2);

      scripted.release();
      expect((await first.wait(BACKGROUND_CONTEXT)).status).toBe("done");
      expect((await second.wait(BACKGROUND_CONTEXT)).status).toBe("done");
      // Each queued turn ran once, in order, with its own input.
      expect(scripted.requests).toEqual(["ledger question", "social question"]);
    } finally {
      scripted.release();
      await harness.close(BACKGROUND_CONTEXT);
      database.close();
    }
  });
});
