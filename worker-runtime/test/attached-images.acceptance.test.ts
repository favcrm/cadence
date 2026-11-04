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
import { createModels, type Models } from "@earendil-works/pi-ai/models";
import { createAssistantMessageEventStream } from "@earendil-works/pi-ai/utils/event-stream";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import type {
  SqliteDatabase,
  SqliteExecutor,
  SqliteValue,
} from "@earendil-works/chord/sqlite";
import { Harness, type Registry } from "@earendil-works/pi-durable";
import { SqliteStorage } from "@earendil-works/pi-durable/storage/sqlite";
import {
  BOARD_MESSAGE_KIND,
  type AppScope,
  type RuntimeTool,
  type ToolContent,
} from "../src/contract.ts";
import { MAX_IMAGE_BASE64_CHARS } from "../src/image-limits.ts";
import { defaultModel } from "../src/model.ts";
import type { OperationScopeSql } from "../src/operation-scope.ts";

/**
 * CAD-2162 independent acceptance: host-attached images on the hosted runtime.
 *
 * This file is not the implementation. It drives the production seams the
 * runtime actually installs:
 *
 * - `operationContextExtension` is the real `GenerationTask.beforeRequest`
 *   hook: it reads the engine's own live run inputs and asks the supplied
 *   `attachedImages` resolver for the active run's images only.
 * - The real `HostedSession` constructor installs the extension and its
 *   operation-id mapping over the engine's placed entries, including the rule
 *   that a placed message id must equal the engine's own request id.
 * - `guardOperationRequests` is the real model-boundary guard on the same
 *   `Models` instance. A hook that throws or cannot attest the attempt means
 *   no provider call at all.
 *
 * The properties:
 * - A valid image reaches the model only for the operation id the engine's own
 *   entry proves, never an unrelated message and never on a replay.
 * - A second input steered into the live run contributes its own image; a
 *   duplicate/replayed message identity makes the attempt unattributable and
 *   never reaches the model.
 * - A throwing resolver (unavailable attachment) and malformed or over-limit
 *   image parts (forged/oversized) never reach the model; the attempt settles
 *   `unanswered`.
 * - Attached image bytes are transient: they never enter the transcript.
 * - A separate capture test leaves the real constructor's metered
 *   `createAI` provider in place and captures `requestModel`, proving the
 *   attached image is serialized as an OpenAI `image_url` data URI for the
 *   active operation, not merely present in a scripted provider's context.
 *
 * Not covered: host-side byte/magic verification and retained-authority
 * revalidation. The contract places those in `attachedImages`; this test uses
 * a resolver to model success, refusal and malformed output, not to assert a
 * decoder. The native daemon validates the retained bytes before they are
 * resolvable at all.
 */

// `hosted-session.ts` imports its Durable Object base from the platform
// module. `bun test` cannot resolve it; the base is stubbed for this file only,
// exactly as the existing app-scope acceptance does.
mock.module("cloudflare:workers", () => ({
  DurableObject: class {
    constructor(
      readonly ctx: unknown,
      readonly env: unknown,
    ) {}
  },
}));

const MODEL: Model<Api> = {
  id: "scripted",
  name: "Scripted",
  api: "openai-completions",
  provider: "scripted",
  baseUrl: "https://scripted.invalid",
  input: ["text", "image"],
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

/** OpenAI-compatible SSE used by the real metered transport in the capture test. */
const frame = (delta: Record<string, unknown>, finish: string | null = null) =>
  `data: ${JSON.stringify({
    id: "synthetic-capture",
    object: "chat.completion.chunk",
    created: 1,
    model: "synthetic-model",
    choices: [{ index: 0, delta, finish_reason: finish }],
  })}\n\n`;

/** Three distinct, real one-pixel PNGs, so leaked content is identifiable. */
const PNG_A =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
const PNG_B =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
const PNG_UNRELATED =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNgYAAAAAMAASsJTYQAAAAASUVORK5CYII=";

const LEDGER: AppScope = { installId: "install_ledger", contextId: "campaign" };

type ImagePart = Extract<ToolContent, { type: "image" }>;
type ResolverInput = {
  operationIds: readonly string[];
  app: AppScope | null;
};
type Resolver = (
  input: ResolverInput,
  signal?: AbortSignal,
) => Promise<readonly ImagePart[]>;
type Observation = {
  readonly messages: readonly Message[];
  readonly user: string;
  readonly hasToolResult: boolean;
};
type Reply = string | ((observation: Observation) => AssistantMessage);

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

function imageParts(messages: readonly Message[]): ImagePart[] {
  return messages.flatMap((message) =>
    Array.isArray(message.content)
      ? message.content.flatMap((part) =>
          part.type === "image" ? [part as ImagePart] : [],
        )
      : [],
  );
}

/**
 * A provider-shaped, offline `Models` instance. Every request's full message
 * array is retained before the provider answers, so a test asserts what the
 * model boundary actually forwarded — not what the resolver said.
 */
function scriptedModels(reply: Reply) {
  const requests: Array<readonly Message[]> = [];
  const respond = (context: { messages: readonly Message[] }) => {
    const observation: Observation = {
      messages: context.messages,
      user: lastUserText(context.messages),
      hasToolResult: context.messages.some(
        (message) => message.role === "toolResult",
      ),
    };
    // Clone so later engine copies cannot retroactively change an observation.
    requests.push(context.messages.map((message) => structuredClone(message)));
    const stream = createAssistantMessageEventStream();
    const message =
      typeof reply === "string" ? assistantReply(reply) : reply(observation);
    queueMicrotask(() => stream.end(message));
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
  return { models, provider, requests };
}

/** `node:sqlite`-shaped adapter over Bun's real SQLite, for these tests only. */
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

/** The engine's own portable SQLite facade over the same Bun database. */
function sqliteFacade(database: Database): SqliteDatabase {
  const executor: SqliteExecutor = {
    async exec(sql: string) {
      database.exec(sql);
    },
    async run(sql: string, ...params: SqliteValue[]) {
      database.prepare(sql).run(...(params as never[]));
    },
    async get<T extends object>(sql: string, ...params: SqliteValue[]) {
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
  const directory = mkdtempSync(join(tmpdir(), "cadence-attached-images-"));
  directories.push(directory);
  const path = join(directory, "session.sqlite");
  return { database: new Database(path), path };
}

/** One operator input exactly as the runtime places it. */
function entry(messageId: string, app?: AppScope) {
  return {
    kind: BOARD_MESSAGE_KIND,
    data:
      app === undefined
        ? { messageId }
        : { messageId, app },
  };
}

/**
 * The real `HostedSession` constructor over the stubbed platform base and a
 * real SQLite. Its installed models, registry, operation-id mapping,
 * `beforeRequest` hook and model-boundary guard are exactly what a production
 * turn runs through; only the model provider is replaced by the scripted
 * offline one — the same seam the existing session-wiring acceptance uses.
 *
 * `resolver` stands in for the host's `attachedImages` implementation. A
 * resolver is host code by contract; this test attacks the runtime's guarantee
 * about which ids it is given and what it does when the host refuses or
 * returns a malformed part.
 */
async function openRig(
  database: Database,
  reply: Reply,
  resolver: Resolver,
  options: { maxImages?: 1 | 2 | 3 | 4; tools?: RuntimeTool[] } = {},
) {
  // Dynamic import is required, not a runtime-selected module: a static
  // import is hoisted above `mock.module("cloudflare:workers")` at the top of
  // this file and fails to resolve `cloudflare:workers` under `bun test`.
  const { HostedSession } = await import("../src/hosted-session.ts");
  const session = new HostedSession(
    {
      id: { name: "conversation" },
      storage: { sql: sqlStore(database) },
    } as never,
    {},
    {
      modelAlias: "scripted",
      requestModel: async () => new Response(null, { status: 500 }),
      attachedImages: resolver,
      maxImagesPerRequest: options.maxImages ?? 2,
      tools:
        options.tools === undefined || options.tools.length === 0
          ? []
          : [{ name: "attachment-test-tools", tools: options.tools }],
      authorizeTool: async () => {},
      toolRefusal: () => undefined,
    },
  ) as unknown as {
    models: Models;
    registry: Registry;
  };
  const scripted = scriptedModels(reply);
  // The guards were installed on the session's own `Models`; replacing the
  // provider leaves those guards in place on the same object.
  session.models.setProvider(scripted.provider);
  const storage = await SqliteStorage.open(sqliteFacade(database), {
    prefix: "pi_",
  });
  const harness = await Harness.open(
    storage,
    { models: session.models, registry: session.registry },
    BACKGROUND_CONTEXT,
  );
  const conversation = await harness.root(BACKGROUND_CONTEXT, {
    agent: { model: { provider: "scripted", modelId: "scripted" } },
  });
  return { scripted, harness, conversation };
}

/**
 * The real constructor with its own metered `createAI` provider left in place:
 * `requestModel` captures the provider payload after `createAI` serialization
 * and `meteredBinding` image bounding. This is the same transport path
 * `tool-image.acceptance.test.ts` exercises, not a scripted provider message.
 */
async function openMeteredRig(
  database: Database,
  resolver: Resolver,
  options: { maxImages?: 1 | 2 | 3 | 4 } = {},
) {
  const requests: Record<string, unknown>[] = [];
  // Dynamic import is required because `cloudflare:workers` is mocked at
  // module scope and a static import would be hoisted above the mock.
  const { HostedSession } = await import("../src/hosted-session.ts");
  const session = new HostedSession(
    {
      id: { name: "conversation" },
      storage: { sql: sqlStore(database) },
    } as never,
    {},
    {
      modelAlias: "scripted",
      requestModel: async (input) => {
        // Capture the wire payload exactly as JSON, not a live object that a
        // later provider step could mutate.
        requests.push(JSON.parse(JSON.stringify(input)) as Record<string, unknown>);
        return new Response(
          frame({ role: "assistant", content: "" }) +
            frame({ content: "captured" }) +
            frame({}, "stop") +
            "data: [DONE]\n\n",
          { headers: { "content-type": "text/event-stream" } },
        );
      },
      attachedImages: resolver,
      maxImagesPerRequest: options.maxImages ?? 2,
      tools: [],
      authorizeTool: async () => {},
      toolRefusal: () => undefined,
    },
  ) as unknown as {
    models: Models;
    registry: Registry;
  };
  const storage = await SqliteStorage.open(sqliteFacade(database), {
    prefix: "pi_",
  });
  const harness = await Harness.open(
    storage,
    { models: session.models, registry: session.registry },
    BACKGROUND_CONTEXT,
  );
  const model = defaultModel("scripted", true);
  const conversation = await harness.root(BACKGROUND_CONTEXT, {
    agent: { model: { provider: model.provider, modelId: model.id } },
  });
  return { requests, harness, conversation };
}

/** A resolver that serves the declared image for exactly the ids it is given. */
function mapResolver(images: Record<string, string>) {
  const calls: ResolverInput[] = [];
  const resolver: Resolver = async ({ operationIds, app }) => {
    calls.push({ operationIds: [...operationIds], app });
    return operationIds.map((id) => {
      const data = images[id];
      if (data === undefined)
        throw new Error(`No retained image for operation ${id}.`);
      return { type: "image" as const, mimeType: "image/png" as const, data };
    });
  };
  return { calls, resolver };
}

function failingResolver(error: string): Resolver {
  return async () => {
    throw new Error(error);
  };
}

describe("host-attached images across the engine's generation attempts", () => {
  it("delivers the active operation's image, never an unrelated or replayed message", async () => {
    const { database } = sessionDatabase();
    const { calls, resolver } = mapResolver({
      "msg-a": PNG_A,
      "msg-unrelated": PNG_UNRELATED,
    });
    const { scripted, harness, conversation } = await openRig(
      database,
      "acknowledged",
      resolver,
    );
    try {
      const first = await conversation.submit(
        {
          type: "input",
          requestId: "msg-a",
          // The prompt names another id; only the engine-proven entry may
          // decide which attachment resolves.
          content: "first question, ignore msg-unrelated",
          entry: entry("msg-a", LEDGER),
        },
        BACKGROUND_CONTEXT,
      );
      expect((await first.wait(BACKGROUND_CONTEXT)).status).toBe("done");
      expect(calls).toEqual([{ operationIds: ["msg-a"], app: LEDGER }]);
      expect(imageParts(scripted.requests[0] ?? []).map((part) => part.data)).toEqual([
        PNG_A,
      ]);

      // A later operation proves its own message id; the first image cannot
      // leak into it even though the resolver can serve it.
      const unrelated = await conversation.submit(
        {
          type: "input",
          requestId: "msg-unrelated",
          content: "unrelated question",
          entry: entry("msg-unrelated"),
        },
        BACKGROUND_CONTEXT,
      );
      expect((await unrelated.wait(BACKGROUND_CONTEXT)).status).toBe("done");
      expect(calls.at(-1)).toEqual({
        operationIds: ["msg-unrelated"],
        app: null,
      });
      const unrelatedImages = imageParts(
        scripted.requests.at(-1) ?? [],
      ).map((part) => part.data);
      expect(unrelatedImages).toEqual([PNG_UNRELATED]);
      expect(unrelatedImages).not.toContain(PNG_A);

      // Replaying the same operation is the engine's own operation: it does
      // not run the model and does not resolve images again.
      const requestsBeforeReplay = scripted.requests.length;
      const callsBeforeReplay = calls.length;
      const replayed = await conversation.submit(
        {
          type: "input",
          requestId: "msg-a",
          content: "first question, ignore msg-unrelated",
          entry: entry("msg-a", LEDGER),
        },
        BACKGROUND_CONTEXT,
      );
      expect((await replayed.wait(BACKGROUND_CONTEXT)).status).toBe("done");
      expect(scripted.requests.length).toBe(requestsBeforeReplay);
      expect(calls.length).toBe(callsBeforeReplay);

      // The images are transient: no provider request is committed as model
      // history the board can read back.
      const records = await conversation.entries(
        { order: "ascending" },
        100,
        undefined,
        BACKGROUND_CONTEXT,
      );
      expect(JSON.stringify(records.items)).not.toContain(PNG_A);
      expect(JSON.stringify(records.items)).not.toContain(PNG_UNRELATED);
    } finally {
      await harness.close(BACKGROUND_CONTEXT);
      database.close();
    }
  }, 30_000);

  it(
    "serializes the active operation's image through the real metered provider transport",
    async () => {
      const { database } = sessionDatabase();
      const { calls, resolver } = mapResolver({
        "msg-a": PNG_A,
        "msg-unrelated": PNG_UNRELATED,
      });
      const { requests, harness, conversation } = await openMeteredRig(
        database,
        resolver,
      );
      const urls = (input: Record<string, unknown>): string[] => {
        const messages = Array.isArray(input.messages)
          ? (input.messages as Array<{ content?: unknown }>)
          : [];
        return messages.flatMap((message) =>
          Array.isArray(message.content)
            ? message.content.flatMap((part) => {
                const value = part as {
                  type?: unknown;
                  image_url?: { url?: unknown };
                };
                return value.type === "image_url" &&
                  typeof value.image_url?.url === "string"
                  ? [value.image_url.url]
                  : [];
              })
            : [],
        );
      };
      const dataUri = (base64: string) => `data:image/png;base64,${base64}`;
      try {
        const first = await conversation.submit(
          {
            type: "input",
            requestId: "msg-a",
            content: "capture question",
            entry: entry("msg-a", LEDGER),
          },
          BACKGROUND_CONTEXT,
        );
        expect((await first.wait(BACKGROUND_CONTEXT)).status).toBe("done");
        expect(calls).toEqual([{ operationIds: ["msg-a"], app: LEDGER }]);
        expect(requests.length).toBeGreaterThan(0);
        // The provider payload is the real createAI/meteredBinding output:
        // an OpenAI image_url data URI, not the engine's raw image part.
        expect(requests.some((request) => urls(request).includes(dataUri(PNG_A)))).toBe(
          true,
        );
        expect(
          requests.every((request) => !JSON.stringify(request).includes(PNG_UNRELATED)),
        ).toBe(true);
        const boundary = requests.length;

        const unrelated = await conversation.submit(
          {
            type: "input",
            requestId: "msg-unrelated",
            content: "unrelated question",
            entry: entry("msg-unrelated"),
          },
          BACKGROUND_CONTEXT,
        );
        expect((await unrelated.wait(BACKGROUND_CONTEXT)).status).toBe("done");
        expect(calls.at(-1)).toEqual({
          operationIds: ["msg-unrelated"],
          app: null,
        });
        const unrelatedRequests = requests.slice(boundary);
        expect(unrelatedRequests.length).toBeGreaterThan(0);
        expect(
          unrelatedRequests.some((request) =>
            urls(request).includes(dataUri(PNG_UNRELATED)),
          ),
        ).toBe(true);
        expect(
          unrelatedRequests.every(
            (request) => !JSON.stringify(request).includes(PNG_A),
          ),
        ).toBe(true);

        // Attached bytes are transient in the real transport path too.
        const records = await conversation.entries(
          { order: "ascending" },
          100,
          undefined,
          BACKGROUND_CONTEXT,
        );
        expect(JSON.stringify(records.items)).not.toContain(PNG_A);
        expect(JSON.stringify(records.items)).not.toContain(PNG_UNRELATED);
      } finally {
        await harness.close(BACKGROUND_CONTEXT);
        database.close();
      }
    },
    30_000,
  );

  it(
    "never resolves images for an input the engine does not prove as a board message",
    async () => {
      const { database } = sessionDatabase();
      let resolverCalls = 0;
      const resolver: Resolver = async () => {
        resolverCalls += 1;
        return [{ type: "image", mimeType: "image/png", data: PNG_A }];
      };
      const { scripted, harness, conversation } = await openRig(
        database,
        "acknowledged",
        resolver,
      );
      try {
        // A pre-board/legacy input has no board message identity. It cannot
        // carry a host attachment binding, so the runtime skips the resolver
        // entirely and runs the turn without image parts.
        const legacy = await conversation.submit(
          {
            type: "input",
            requestId: "op-legacy",
            content: "legacy input",
            entry: { kind: "board.note", data: { note: "legacy" } },
          },
          BACKGROUND_CONTEXT,
        );
        expect((await legacy.wait(BACKGROUND_CONTEXT)).status).toBe("done");
        expect(resolverCalls).toBe(0);
        expect(scripted.requests).toHaveLength(1);
        expect(imageParts(scripted.requests[0] ?? [])).toHaveLength(0);
      } finally {
        await harness.close(BACKGROUND_CONTEXT);
        database.close();
      }
    },
    30_000,
  );

  it(
    "resolves images for every admitted input of a steered run, in engine order",
    async () => {
      const { database } = sessionDatabase();
      const { calls, resolver } = mapResolver({
        "msg-a": PNG_A,
        "msg-b": PNG_B,
        "msg-unrelated": PNG_UNRELATED,
      });
      const toolStarted = Promise.withResolvers<void>();
      const toolGate = Promise.withResolvers<void>();
      const slow: RuntimeTool = {
        name: "slow_probe",
        description: "Hold the run open while another admitted input steers in.",
        parameters: { type: "object", properties: {} },
        async execute() {
          toolStarted.resolve();
          await toolGate.promise;
          return { output: [{ type: "text", text: "noted" }] };
        },
      };
      const { scripted, harness, conversation } = await openRig(
        database,
        ({ hasToolResult }) =>
          hasToolResult
            ? assistantReply("finished")
            : toolCallReply("slow-call", "slow_probe"),
        resolver,
        { tools: [slow] },
      );
      try {
        const running = await conversation.submit(
          {
            type: "input",
            requestId: "msg-a",
            content: "first admitted input",
            entry: entry("msg-a"),
          },
          BACKGROUND_CONTEXT,
        );
        await toolStarted.promise;
        expect(calls).toEqual([{ operationIds: ["msg-a"], app: null }]);
        const steer = await conversation.submit(
          {
            type: "input",
            requestId: "msg-b",
            content: "second admitted input",
            whenBusy: "steer",
            entry: entry("msg-b"),
          },
          BACKGROUND_CONTEXT,
        );
        toolGate.resolve();
        await harness.waitForIdle(BACKGROUND_CONTEXT);
        await running.wait(BACKGROUND_CONTEXT);
        await steer.wait(BACKGROUND_CONTEXT);

        // Each attempt resolves only the run's own roots. The second attempt
        // sees both placed inputs, in engine order, and never the unrelated id
        // the resolver could have served.
        expect(calls).toEqual([
          { operationIds: ["msg-a"], app: null },
          { operationIds: ["msg-a", "msg-b"], app: null },
        ]);
        expect(scripted.requests).toHaveLength(2);
        expect(
          imageParts(scripted.requests[0] ?? []).map((part) => part.data),
        ).toEqual([PNG_A]);
        expect(
          imageParts(scripted.requests[1] ?? []).map((part) => part.data),
        ).toEqual([PNG_A, PNG_B]);
        expect(JSON.stringify(scripted.requests)).not.toContain(PNG_UNRELATED);
      } finally {
        toolGate.resolve();
        await harness.close(BACKGROUND_CONTEXT);
        database.close();
      }
    },
    30_000,
  );

  it(
    "never calls the model when the host resolver cannot supply an attachment",
    async () => {
      const { database } = sessionDatabase();
      const { scripted, harness, conversation } = await openRig(
        database,
        "must not run",
        failingResolver("The retained attachment is unavailable."),
      );
      try {
        for (const [operationId, app] of [
          ["msg-home-unavailable", undefined],
          ["msg-app-unavailable", LEDGER],
        ] as const) {
          const turn = await conversation.submit(
            {
              type: "input",
              requestId: operationId,
              content: `question ${operationId}`,
              entry: entry(operationId, app),
            },
            BACKGROUND_CONTEXT,
          );
          await harness.waitForIdle(BACKGROUND_CONTEXT);
          expect(scripted.requests).toHaveLength(0);
          expect((await turn.status(BACKGROUND_CONTEXT)).status).toBe(
            "unanswered",
          );
        }
      } finally {
        await harness.close(BACKGROUND_CONTEXT);
        database.close();
      }
    },
    30_000,
  );

  it(
    "never calls the model for a forged, malformed or over-limit image part",
    async () => {
      const oversized = "A".repeat(MAX_IMAGE_BASE64_CHARS + 4);
      const cases: Array<{
        readonly name: string;
        readonly images: readonly unknown[];
        readonly maxImages?: 1 | 2 | 3 | 4;
      }> = [
        {
          name: "wrong MIME",
          images: [{ type: "image", mimeType: "image/gif", data: PNG_A }],
        },
        {
          name: "not base64",
          images: [
            { type: "image", mimeType: "image/png", data: "not base64!" },
          ],
        },
        {
          name: "more parts than the configured limit",
          images: [PNG_A, PNG_B, PNG_UNRELATED].map((data) => ({
            type: "image",
            mimeType: "image/png",
            data,
          })),
          maxImages: 2,
        },
        {
          name: "over the whole-request base64 budget",
          images: [{ type: "image", mimeType: "image/png", data: oversized }],
        },
      ];
      for (const testCase of cases) {
        const { database } = sessionDatabase();
        const resolver: Resolver = async () =>
          testCase.images as readonly ImagePart[];
        const { scripted, harness, conversation } = await openRig(
          database,
          "must not run",
          resolver,
          { maxImages: testCase.maxImages },
        );
        try {
          const turn = await conversation.submit(
            {
              type: "input",
              requestId: "msg-forged",
              content: "inspect the forged attachment",
              entry: entry("msg-forged", LEDGER),
            },
            BACKGROUND_CONTEXT,
          );
          await harness.waitForIdle(BACKGROUND_CONTEXT);
          expect(scripted.requests, testCase.name).toHaveLength(0);
          expect(
            (await turn.status(BACKGROUND_CONTEXT)).status,
            testCase.name,
          ).toBe("unanswered");
        } finally {
          await harness.close(BACKGROUND_CONTEXT);
          database.close();
        }
      }
    },
    30_000,
  );

  it(
    "refuses a second attempt when a steered input forges the message identity",
    async () => {
      const { database } = sessionDatabase();
      const { calls, resolver } = mapResolver({ "msg-a": PNG_A });
      const toolStarted = Promise.withResolvers<void>();
      const toolGate = Promise.withResolvers<void>();
      const slow: RuntimeTool = {
        name: "slow_probe",
        description: "Hold the run open so a replayed message id can steer in.",
        parameters: { type: "object", properties: {} },
        async execute() {
          toolStarted.resolve();
          await toolGate.promise;
          return { output: [{ type: "text", text: "noted" }] };
        },
      };
      const { scripted, harness, conversation } = await openRig(
        database,
        ({ hasToolResult }) =>
          hasToolResult
            ? assistantReply("must not be requested")
            : toolCallReply("slow-call", "slow_probe"),
        resolver,
        { tools: [slow] },
      );
      try {
        const running = await conversation.submit(
          {
            type: "input",
            requestId: "msg-a",
            content: "first admitted input",
            entry: entry("msg-a"),
          },
          BACKGROUND_CONTEXT,
        );
        await toolStarted.promise;
        expect(calls).toEqual([{ operationIds: ["msg-a"], app: null }]);

        // A different engine request carrying the first operation's message
        // identity is a forged/replayed binding, not a second attachment.
        // The real operation-id mapping requires the placed message id to be
        // the engine's own request id, refuses the mismatch, and the hook then
        // attests nothing — so the boundary never calls the model again.
        const steer = await conversation.submit(
          {
            type: "input",
            requestId: "msg-b",
            content: "second admitted input",
            whenBusy: "steer",
            entry: entry("msg-a"),
          },
          BACKGROUND_CONTEXT,
        );
        toolGate.resolve();
        await harness.waitForIdle(BACKGROUND_CONTEXT);

        // Only the first attempt ran; the post-tool attempt was refused before
        // the resolver and before the provider.
        expect(scripted.requests).toHaveLength(1);
        expect(calls).toHaveLength(1);
        expect((await running.status(BACKGROUND_CONTEXT)).status).toBe(
          "unanswered",
        );
        expect((await steer.status(BACKGROUND_CONTEXT)).status).toBe(
          "unanswered",
        );
      } finally {
        toolGate.resolve();
        await harness.close(BACKGROUND_CONTEXT);
        database.close();
      }
    },
    30_000,
  );
});
