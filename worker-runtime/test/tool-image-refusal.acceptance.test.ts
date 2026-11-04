import { Database } from "bun:sqlite";
import { afterEach, describe, expect, it, mock } from "bun:test";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import type {
  SqliteDatabase,
  SqliteExecutor,
  SqliteValue,
} from "@earendil-works/chord/sqlite";
import { Harness, watchEvents } from "@earendil-works/pi-durable";
import { SqliteStorage } from "@earendil-works/pi-durable/storage/sqlite";
import { BOARD_MESSAGE_KIND } from "../src/contract.ts";
import { defaultModel } from "../src/model.ts";

/**
 * Independently authored refusal acceptance for CAD-1977's hosted-runtime
 * bullets, written from the ticket by a reviewer, not the implementer. The
 * implementer may not edit or weaken it (AGENTS.md "Gates and security work").
 *
 * Every case runs the real `HostedSession` constructor, so the wiring under
 * test is what a host gets: the opt-in, the image limits, the live authority
 * and the app-scope guard, over the real engine, real SQLite and the real model
 * adapter. Only the transport answers are scripted.
 *
 * Not covered: the Workers runtime itself. `PiHarness` needs a Durable Object
 * lifecycle, so the session's `harness` field is pointed at the engine directly.
 * The `/events` route, its serializer and the transcript read are the session's
 * own and run unchanged.
 */

// `mock.module` is process-wide and `hosted-session.ts` is cached after its
// first import. Keep this stub identical to the ones in fresh-start.test.ts and
// app-scope.acceptance.test.ts (it must carry `ctx`).
mock.module("cloudflare:workers", () => ({
  DurableObject: class {
    constructor(
      readonly ctx: unknown,
      readonly env: unknown,
    ) {}
  },
}));

// A synthetic one-pixel PNG; no browser or provider is involved.
const PNG =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
const UNVALIDATED = "UNVALIDATED";
const ANSWER = "answered in text";
const RUN_END = '"type":"run_end"';

const chunk = (delta: Record<string, unknown>, finish: string | null = null) =>
  `data: ${JSON.stringify({
    id: "synthetic-response",
    object: "chat.completion.chunk",
    created: 1,
    model: "synthetic-model",
    choices: [{ index: 0, delta, finish_reason: finish }],
  })}\n\n`;
const sse = (body: string) =>
  new Response(`${body}data: [DONE]\n\n`, {
    headers: { "content-type": "text/event-stream" },
  });
const callCapture = () =>
  sse(
    chunk({ role: "assistant", content: "" }) +
      chunk({
        tool_calls: [
          {
            index: 0,
            id: "capture-1",
            type: "function",
            function: { name: "capture", arguments: "{}" },
          },
        ],
      }) +
      chunk({}, "tool_calls"),
  );
const answer = () =>
  sse(
    chunk({ role: "assistant", content: "" }) +
      chunk({ content: ANSWER }) +
      chunk({}, "stop"),
  );
/** The tool on the first request, then the answer: a model that takes images. */
const captureThenAnswer = (request: number) =>
  request === 1 ? callCapture() : answer();

const image = (data = PNG, mimeType = "image/png") => ({
  type: "image",
  data,
  mimeType,
});

function sqlStore(database: Database) {
  return {
    exec(query: string, ...bindings: (string | number | null)[]) {
      if (/^\s*(SELECT|PRAGMA|WITH)/i.test(query)) {
        const rows = database.prepare(query).all(...bindings);
        return { toArray: () => rows };
      }
      database.prepare(query).run(...bindings);
      return { toArray: () => [] };
    },
  };
}

/** The engine's portable SQLite facade over the session's one Bun database. */
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
    async close() {},
  };
}

/** Everything the `/events` route has sent so far, and a wait for a marker. */
function drain(response: Response) {
  const reader = response.body!.getReader();
  const decoder = new TextDecoder();
  let text = "";
  const waiters: Array<() => void> = [];
  const pump = (async () => {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) return;
      text += decoder.decode(value, { stream: true });
      for (const wake of waiters.splice(0)) wake();
    }
  })().catch(() => {});
  return {
    text: () => text,
    /** Resolves once `marker` has appeared `times` times in what was sent. */
    async until(marker: string, times = 1) {
      // The timeout is a failure safeguard only: the wake is the frame arriving.
      const deadline = Date.now() + 10_000;
      while (text.split(marker).length - 1 < times) {
        const left = deadline - Date.now();
        if (left <= 0) throw new Error(`/events never sent ${marker}`);
        await new Promise<void>((resolve) => {
          const timer = setTimeout(resolve, left);
          waiters.push(() => {
            clearTimeout(timer);
            resolve();
          });
        });
      }
    },
    async stop() {
      await reader.cancel();
      await pump;
    },
  };
}

type ToolResultMessage = { role: string; content: unknown[]; isError?: boolean };

const cleanups: Array<() => Promise<void>> = [];
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup();
});

/**
 * A constructed session with one `capture` tool returning `output`, its
 * `/events` route open from before the first turn.
 */
async function openSession(
  output: readonly unknown[],
  options: Record<string, unknown>,
  reply: (request: number, input: Record<string, unknown>) => Response = captureThenAnswer,
  authorizeTool: () => Promise<void> = async () => {},
) {
  const { HostedSession } = await import("../src/hosted-session.ts");
  const requests: Record<string, unknown>[] = [];
  const handled = { count: 0 };
  const database = new Database(":memory:");
  const session = new HostedSession(
    {
      id: { name: "conversation" },
      storage: { sql: sqlStore(database) },
    } as never,
    {} as never,
    {
      modelAlias: "default",
      requestModel: async (input: Record<string, unknown>) => {
        requests.push(input);
        return reply(requests.length, input);
      },
      tools: [
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
                handled.count += 1;
                return { output };
              },
            },
          ],
        },
      ],
      authorizeTool,
      toolRefusal: () => undefined,
      ...options,
    } as never,
  );
  const wired = session as unknown as {
    models: never;
    registry: never;
    harness: unknown;
  };
  const harness = await Harness.open(
    await SqliteStorage.open(sqliteFacade(database), { prefix: "pi_" }),
    { models: wired.models, registry: wired.registry },
    BACKGROUND_CONTEXT,
  );
  cleanups.push(async () => {
    await harness.close(BACKGROUND_CONTEXT);
    database.close();
  });
  const conversation = await harness.root(BACKGROUND_CONTEXT, {
    agent: {
      model: { provider: defaultModel("default").provider, modelId: "default" },
    },
  });
  // Only the Durable Object lifecycle is replaced: the same two engine calls
  // `PiHarness` answers `/events` and the transcript read with.
  wired.harness = {
    session: () => ({
      id: conversation.id,
      events: () => watchEvents(harness, conversation.id, BACKGROUND_CONTEXT),
    }),
    conversation: async () => conversation,
  };
  const openEvents = async () => {
    const response = await session.fetch(
      new Request("https://session.internal/events"),
    );
    expect(response.status).toBe(200);
    return drain(response);
  };
  const live = await openEvents();
  cleanups.push(() => live.stop());
  let operations = 0;
  return {
    requests,
    handled,
    live,
    openEvents,
    session,
    /** One board turn, returned once `/events` has sent its end. */
    async turn(content: string) {
      const operationId = `op-${(operations += 1)}`;
      const ended = live.text().split(RUN_END).length - 1;
      const operation = await conversation.submit(
        {
          type: "input",
          requestId: operationId,
          content,
          entry: { kind: BOARD_MESSAGE_KIND, data: { messageId: operationId } },
        },
        BACKGROUND_CONTEXT,
      );
      await operation.wait(BACKGROUND_CONTEXT);
      await live.until(RUN_END, ended + 1);
    },
    async stored() {
      const items = (
        await conversation.entries(
          { order: "ascending" },
          100,
          undefined,
          BACKGROUND_CONTEXT,
        )
      ).items;
      return items.flatMap((record) => record.model ?? []);
    },
    async toolResult() {
      const messages = await this.stored();
      return messages.find(
        (message) => message.role === "toolResult",
      ) as unknown as ToolResultMessage;
    },
  };
}

describe("hosted image refusals through a constructed session", () => {
  it("delivers image bytes to the model with the opt-in, and only to the model", async () => {
    const run = await openSession([image()], { acceptsToolImages: true });
    await run.turn("Inspect this page.");
    expect(run.handled.count).toBe(1);
    expect(JSON.stringify(run.requests[1])).toContain(
      `data:image/png;base64,${PNG}`,
    );
    // The bytes are in the private record, so their absence below means something.
    expect(JSON.stringify(await run.toolResult())).toContain(PNG);
    expect(JSON.stringify(await run.session.transcript(true))).not.toContain(PNG);
    expect(run.live.text()).not.toContain(PNG);
  });

  it("serves no image bytes on the real /events route, live or on reconnect", async () => {
    const run = await openSession(
      [{ type: "text", text: "Synthetic page capture." }, image()],
      { acceptsToolImages: true },
    );
    await run.turn("Inspect this page.");
    expect(JSON.stringify(await run.toolResult())).toContain(PNG);
    const live = run.live.text();
    expect(live).toContain("Synthetic page capture.");
    expect(live).toContain('"redacted":true');
    expect(live).not.toContain(PNG);
    // The engine's own watch does carry the bytes: the route is what removes them.
    const watch = await watchEventsOf(run);
    expect(watch).toContain(PNG);
    const reconnect = await run.openEvents();
    try {
      await reconnect.until('"type":"snapshot"');
      await reconnect.until('"redacted":true');
      expect(reconnect.text()).not.toContain(PNG);
    } finally {
      await reconnect.stop();
    }
  });

  for (const [name, options] of [
    ["absent", {}],
    ["false", { acceptsToolImages: false }],
    ["truthy but not true", { acceptsToolImages: "true" }],
  ] as const) {
    it(`refuses an image result when the host's opt-in is ${name}`, async () => {
      const run = await openSession([image()], options);
      await run.turn("Inspect this page.");
      const result = await run.toolResult();
      expect(result.isError).toBe(true);
      expect(JSON.stringify(result)).not.toContain(PNG);
      expect(JSON.stringify(run.requests)).not.toContain(PNG);
      expect(run.live.text()).not.toContain(PNG);
      expect(
        JSON.stringify(await run.session.transcript(true)),
      ).toContain("Image input is unavailable for this session.");
    });
  }

  it("refuses a result over the part count or the byte budget", async () => {
    for (const output of [
      Array.from({ length: 5 }, () => image("AAAA")),
      [image("A".repeat(100 * 1024)), image("A".repeat(100 * 1024))],
    ]) {
      const run = await openSession(output, { acceptsToolImages: true });
      await run.turn("Inspect this page.");
      expect((await run.toolResult()).isError).toBe(true);
      expect(JSON.stringify(run.requests[1])).not.toContain("image_url");
    }
  });

  it("does not reach the handler or the model for a revoked call", async () => {
    const run = await openSession(
      [image()],
      { acceptsToolImages: true },
      captureThenAnswer,
      async () => {
        throw new Error("session_revoked");
      },
    );
    await run.turn("Inspect this page.");
    expect(run.handled.count).toBe(0);
    expect(JSON.stringify(run.requests)).not.toContain(PNG);
    expect(run.live.text()).not.toContain(PNG);
  });

  it("never turns a part that is neither text nor a valid image into image input or stored bytes", async () => {
    for (const options of [{}, { acceptsToolImages: true }]) {
      const run = await openSession(
        [{ type: "file", data: UNVALIDATED }],
        options,
      );
      await run.turn("Inspect this page.");
      const result = await run.toolResult();
      expect(result.isError).toBe(true);
      expect(JSON.stringify(result)).not.toContain(UNVALIDATED);
      expect(JSON.stringify(run.requests)).not.toContain(UNVALIDATED);
      expect(run.live.text()).not.toContain(UNVALIDATED);
    }
  });

  it("ends a turn holding the newest image on an explicit refusal, then answers a later text turn without the bytes", async () => {
    const run = await openSession(
      [{ type: "text", text: "Synthetic page capture." }, image()],
      { acceptsToolImages: true },
      (request, input) => {
        if (request === 1) return callCapture();
        if (JSON.stringify(input).includes("image_url"))
          return Response.json(
            {
              error: {
                message: "PRIVATE provider detail",
                type: "invalid_request_error",
                code: "image_input_unavailable",
              },
            },
            { status: 404 },
          );
        return answer();
      },
    );
    await run.turn("Inspect this page.");
    // The real session's context messages must not hide the image from the
    // refusal: no answer may be invented for the image the model was just given.
    expect(run.requests).toHaveLength(2);
    const refused = JSON.stringify(await run.session.transcript(true));
    expect(refused).toContain("The selected model cannot read images.");
    expect(refused).not.toContain(ANSWER);
    expect(refused).not.toContain("PRIVATE");

    await run.turn("Forget the image and say hello.");
    const recovered = JSON.stringify(await run.session.transcript(true));
    expect(recovered).toContain(ANSWER);
    const sent = JSON.stringify(run.requests.at(-1));
    expect(sent).not.toContain(PNG);
    expect(sent).toMatch(/image.{0,100}omitted|omitted.{0,100}image/i);
    // The model input changed; the private record did not.
    expect(JSON.stringify(await run.toolResult())).toContain(PNG);
    expect(run.live.text()).not.toContain(PNG);
  });
});

/** The engine's own serialized snapshot, before the route's serializer. */
async function watchEventsOf(run: {
  session: unknown;
}): Promise<string> {
  const harness = (
    run.session as {
      harness: {
        session(): {
          events(): Promise<{
            snapshot: unknown;
            stop(): Promise<unknown>;
          }>;
        };
      };
    }
  ).harness;
  const watch = await harness.session().events();
  const text = JSON.stringify(watch.snapshot);
  await watch.stop();
  return text;
}
