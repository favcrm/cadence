import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import { createModels } from "@earendil-works/pi-ai/models";
import {
  type Cursor,
  createRegistry,
  Harness,
  MemoryStorage,
  ResetEntry,
} from "@earendil-works/pi-durable";
import { describe, expect, it } from "bun:test";
import {
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

  it("names the engine's reset record", () => {
    expect(RESET_KIND).toBe(ResetEntry.kind);
  });
});
