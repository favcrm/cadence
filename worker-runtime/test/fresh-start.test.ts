import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import { createModels } from "@earendil-works/pi-ai/models";
import {
  type Cursor,
  createRegistry,
  Harness,
  CompactionEntry,
  MemoryStorage,
  ResetEntry,
} from "@earendil-works/pi-durable";
import { describe, expect, it, mock } from "bun:test";
import {
  COMPACTION_KIND,
  FRESH_START_TEXT,
  RESET_KIND,
  readAllRecords,
  threadEntries,
} from "../src/transcript.ts";

/**
 * A fresh start over the real engine: the model's context narrows, while the
 * board's thread keeps its rows and its `seq` only ever grows. An open stream
 * drops anything at or below its cursor, and the board merges by `seq`, so a
 * restart would hide or overwrite messages.
 */
describe("fresh start", () => {
  const say = (text: string) => ({
    type: "write" as const,
    entry: {
      kind: "pi.user",
      model: [{ role: "user" as const, content: text, timestamp: 1000 }],
    },
  });

  it("keeps the thread and its numbering, and narrows only the model's context", async () => {
    const harness = await Harness.open(
      new MemoryStorage(),
      { models: createModels({}), registry: createRegistry() },
      BACKGROUND_CONTEXT,
    );
    try {
      const conversation = await harness.root(BACKGROUND_CONTEXT, {
        agent: { model: { provider: "scripted", modelId: "scripted" } },
      });
      const write = async (request: ReturnType<typeof say>) => {
        const operation = await conversation.submit(request, BACKGROUND_CONTEXT);
        await operation.wait(BACKGROUND_CONTEXT);
      };
      const board = async () =>
        threadEntries(
          await readAllRecords<Cursor>((cursor, limit) =>
            conversation.entries(
              { order: "ascending" },
              limit,
              cursor,
              BACKGROUND_CONTEXT,
            ),
          ),
          false,
        );

      await write(say("first"));
      await write(say("second"));
      const before = await board();
      expect(before.map((entry) => entry.text)).toEqual(["first", "second"]);

      await conversation.reset(undefined, BACKGROUND_CONTEXT);
      await write(say("third"));

      const after = await board();
      expect(after.map((entry) => entry.text)).toEqual([
        "first",
        "second",
        FRESH_START_TEXT,
        "third",
      ]);
      // The numbering an open stream and the board's merge rely on.
      expect(after.map((entry) => entry.seq)).toEqual([1, 2, 3, 4]);
      expect(after.slice(0, 2)).toEqual(before);

      const context = await conversation.context(BACKGROUND_CONTEXT);
      const seen = context.entries.flatMap((record) => record.model ?? []);
      expect(seen.map((message) => message.content)).toEqual(["third"]);
    } finally {
      await harness.close(BACKGROUND_CONTEXT);
    }
  });

  it("reads every page of a long history in order", async () => {
    const records = Array.from({ length: 1201 }, (_, index) => ({
      id: index + 1,
    })) as never[];
    const read = await readAllRecords<number>(async (cursor, limit) => {
      const start = cursor ?? 0;
      const next = start + limit;
      return {
        items: records.slice(start, next),
        ...(next < records.length ? { next } : {}),
      };
    });
    expect(read).toEqual(records);
  });

  it("marks nothing when a thread starts with a reset", () => {
    expect(
      threadEntries(
        [{ id: 1, conversationId: 1, kind: RESET_KIND }] as never,
        false,
      ),
    ).toEqual([]);
  });

  it("names the engine's reset and compaction records", () => {
    expect(RESET_KIND).toBe(ResetEntry.kind);
    expect(COMPACTION_KIND).toBe(CompactionEntry.kind);
  });

  const failed = (id: number, stopReason = "error", errorMessage = "prompt is too long: context overflow") =>
    ({
      id,
      conversationId: 1,
      kind: "pi.assistant",
      model: [
        {
          role: "assistant",
          content: [],
          stopReason,
          errorMessage,
          timestamp: 1000,
          usage: {},
        },
      ],
    }) as never;
  const user = (id: number, text: string) =>
    ({
      id,
      conversationId: 1,
      kind: "pi.user",
      model: [{ role: "user", content: text, timestamp: 1000 }],
    }) as never;
  const reset = (id: number) =>
    ({ id, conversationId: 1, kind: RESET_KIND, head: id }) as never;
  const texts = (entries: ReturnType<typeof threadEntries>) =>
    entries.map((entry) => `${entry.seq} ${entry.kind} ${entry.text}`);

  it("settles a hidden failure at the reset, in one stable order", () => {
    const hidden = threadEntries([user(1, "first"), failed(2)], false);
    expect(hidden.map((entry) => entry.seq)).toEqual([1]);
    const before = threadEntries(
      [user(1, "first"), failed(2), reset(3)],
      false,
    );
    const after = threadEntries(
      [user(1, "first"), failed(2), reset(3), user(4, "next")],
      false,
    );
    expect(after.slice(0, before.length)).toEqual(before);
    expect(after.map((entry) => entry.kind)).toEqual([
      "message",
      "turn_result",
      "message",
      "message",
    ]);
    expect(after[2]?.text).toBe(FRESH_START_TEXT);
  });

  it("collapses resets in a row into one marker", () => {
    expect(
      texts(
        threadEntries(
          [user(1, "first"), reset(2), reset(3), reset(4), user(5, "next")],
          false,
        ),
      ),
    ).toEqual([
      "1 message first",
      `2 message ${FRESH_START_TEXT}`,
      "3 message next",
    ]);
  });

  it("does not show a compaction's summary, or call its recovered overflow a failure", () => {
    const compaction = {
      id: 3,
      conversationId: 1,
      kind: COMPACTION_KIND,
      head: 1,
      model: [{ role: "user", content: "<summary>", timestamp: 1000 }],
    } as never;
    const entries = threadEntries(
      [user(1, "first"), failed(2), compaction],
      false,
    );
    expect(texts(entries)).toEqual(["1 message first"]);
  });

  it("numbers a reconnect the same as a page read", async () => {
    // `mock.module` is process-wide and `hosted-session.ts` is cached after its
    // first import, so whichever file loads it first fixes the base class for
    // every file. Keep this stub identical to the one in
    // app-scope.acceptance.test.ts (it must carry `ctx`), or the later file's
    // constructed sessions fail depending on the runner's file order.
    mock.module("cloudflare:workers", () => ({
      DurableObject: class {
        constructor(
          readonly ctx: unknown,
          readonly env: unknown,
        ) {}
      },
    }));
    const { HostedSession } = await import("../src/hosted-session.ts");
    const harness = await Harness.open(
      new MemoryStorage(),
      { models: createModels({}), registry: createRegistry() },
      BACKGROUND_CONTEXT,
    );
    try {
      const conversation = await harness.root(BACKGROUND_CONTEXT, {
        agent: { model: { provider: "scripted", modelId: "scripted" } },
      });
      const write = async (text: string) => {
        const operation = await conversation.submit(say(text), BACKGROUND_CONTEXT);
        await operation.wait(BACKGROUND_CONTEXT);
      };
      await write("first");
      await write("second");
      await conversation.reset(undefined, BACKGROUND_CONTEXT);
      for (const text of ["third", "fourth", "fifth", "sixth"]) await write(text);

      const session = Object.assign(Object.create(HostedSession.prototype), {
        ctx: { storage: { get: async () => undefined } },
        options: { modelAlias: "alias" },
        harness: {
          conversation: async () => conversation,
          pending: async () => [],
          session: () => ({
            id: "1",
            // The engine's own snapshot: the narrowed active context.
            events: async () => {
              const closed = Promise.withResolvers<{ reason: "session_closed" }>();
              const { entries } = await conversation.context(BACKGROUND_CONTEXT);
              return {
                snapshot: { type: "snapshot", entries, tools: [] },
                start: () => closed.resolve({ reason: "session_closed" }),
                stop: async () => ({ reason: "cancelled" }),
                closed: closed.promise,
              };
            },
          }),
        },
      }) as InstanceType<typeof HostedSession>;

      const frames = async (after: number) => {
        const response = await session.fetch(
          new Request(`https://board.test/thread-stream?after=${after}`),
        );
        expect(response.status).toBe(200);
        const body = await response.text();
        return body
          .split("\n\n")
          .filter((frame) => frame.includes("event: entry"))
          .map((frame) => {
            const entry = JSON.parse(frame.slice(frame.indexOf("data: ") + 6));
            return `${entry.seq} ${entry.text}`;
          });
      };
      const page = (await session.transcript(false)).map(
        (entry) => `${entry.seq} ${entry.text}`,
      );
      expect(page).toEqual([
        "1 first",
        "2 second",
        `3 ${FRESH_START_TEXT}`,
        "4 third",
        "5 fourth",
        "6 fifth",
        "7 sixth",
      ]);
      // Above the narrowed length: a tab that missed the newest rows gets them.
      expect(await frames(5)).toEqual(page.slice(5));
      // Across the reset: the right rows, never other text at a held number.
      expect(await frames(2)).toEqual(page.slice(2));
      expect(await frames(0)).toEqual(page);
    } finally {
      await harness.close(BACKGROUND_CONTEXT);
    }
  });
});
