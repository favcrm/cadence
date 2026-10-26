import { describe, expect, it } from "bun:test";
import { readGenerationIdentity, type TaskSql } from "../src/identity.ts";

const sql = (rows: Record<string, unknown>[]): TaskSql => ({
  exec: () => ({ toArray: () => rows }),
});

const record = (
  checkpoint: unknown,
  kind = "pi.generation",
  status = "running",
) => JSON.stringify({ conversationId: 1, kind, state: { status, checkpoint } });

describe("generation identity adapter", () => {
  it("reads the one running request checkpoint", () => {
    expect(
      readGenerationIdentity(
        sql([
          {
            id: 9,
            record: record({ phase: "request", cutoff: 12, attempt: 2 }),
          },
        ]),
      ),
    ).toEqual({ conversation: "1", task: "9", cutoff: "12" });
  });

  it("fails closed on zero, multiple, or non-request checkpoints", () => {
    expect(readGenerationIdentity(sql([]))).toBeNull();
    expect(
      readGenerationIdentity(
        sql([
          { id: 9, record: record({ phase: "request", cutoff: 12 }) },
          { id: 10, record: record({ phase: "request", cutoff: 13 }) },
        ]),
      ),
    ).toBeNull();
    expect(
      readGenerationIdentity(
        sql([{ id: 9, record: record({ phase: "prepare", attempt: 1 }) }]),
      ),
    ).toBeNull();
    expect(
      readGenerationIdentity(
        sql([
          {
            id: 9,
            record: record({ phase: "request", cutoff: 12 }, "pi.tool"),
          },
        ]),
      ),
    ).toBeNull();
  });

  it("fails closed on a malformed record or unusable id", () => {
    expect(readGenerationIdentity(sql([{ id: 9, record: "{" }]))).toBeNull();
    expect(
      readGenerationIdentity(
        sql([
          { id: 9, record: record({ phase: "request", cutoff: "bad value" }) },
        ]),
      ),
    ).toBeNull();
    expect(
      readGenerationIdentity(
        sql([
          {
            id: 9,
            record: record(
              { phase: "request", cutoff: "12" },
              "pi.generation",
              "done",
            ),
          },
        ]),
      ),
    ).toBeNull();
  });

  it("fails closed when the durable table cannot be read", () => {
    expect(
      readGenerationIdentity({
        exec() {
          throw new Error("no such table: pi_tasks");
        },
      }),
    ).toBeNull();
  });

  it("accepts a numeric or string cutoff only in the identity's alphabet", () => {
    expect(
      readGenerationIdentity(
        sql([{ id: "17", record: record({ phase: "request", cutoff: 0 }) }]),
      ),
    ).toEqual({ conversation: "1", task: "17", cutoff: "0" });
    expect(
      readGenerationIdentity(
        sql([
          {
            id: 9,
            record: JSON.stringify({
              conversationId: "1/2",
              kind: "pi.generation",
              state: {
                status: "running",
                checkpoint: { phase: "request", cutoff: 3 },
              },
            }),
          },
        ]),
      ),
    ).toBeNull();
  });
});
