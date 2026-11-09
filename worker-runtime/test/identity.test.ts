import { describe, expect, it } from "bun:test";
import { readCompactionFailure, readCompactionIdentity, readGenerationIdentity, type TaskSql } from "../src/identity.ts";

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

describe("failed compaction adapter", () => {
  const task = (
    id: number,
    conversationId: number,
    outcome: unknown,
    owner: unknown = 5,
  ) => ({
    id,
    record: JSON.stringify({
      conversationId,
      kind: "pi.compaction",
      ...(owner === null ? {} : { owner }),
      state: { status: "terminal", outcome },
    }),
  });
  /** The compaction rows, and the owning generation's own status. */
  const tasks = (rows: Record<string, unknown>[], ownerStatus = "terminal"): TaskSql => ({
    exec(query) {
      return {
        toArray: () =>
          query.startsWith("SELECT status") ? [{ status: ownerStatus }] : rows,
      };
    },
  });
  const failed = (id: number, conversationId: number, outcome: unknown = { status: "failed" }) =>
    task(id, conversationId, outcome);

  it("reports a blocking failure once its owning generation settled", () => {
    expect(readCompactionFailure(tasks([failed(30, 1)]), 1)).toBe(true);
    expect(readCompactionFailure(tasks([failed(30, 1)], "running"), 1)).toBe(false);
    expect(readCompactionFailure(tasks([failed(30, 1, { status: "faulted" })]), 1)).toBe(true);
  });

  it("ignores a conversation-owned background failure", () => {
    expect(readCompactionFailure(tasks([task(30, 1, { status: "failed" }, null)]), 1))
      .toBe(false);
  });

  it("fails closed on a succeeded, malformed or unreadable task", () => {
    expect(readCompactionFailure(tasks([failed(30, 1, { status: "completed" })]), 1))
      .toBe(false);
    expect(readCompactionFailure(tasks([{ id: 30, record: "{" }]), 1)).toBe(false);
    expect(readCompactionFailure(tasks([]), 1)).toBe(false);
    expect(
      readCompactionFailure(
        {
          exec() {
            throw new Error("no such table: pi_tasks");
          },
        },
        1,
      ),
    ).toBe(false);
  });
});

describe("compaction identity adapter", () => {
  const row = (conversationId: unknown, tail: unknown, id: unknown = 9, owner: unknown = 1) => ({
    id: 9,
    conversation_id: owner,
    record: JSON.stringify({
      id,
      conversationId,
      kind: "pi.compaction",
      state: { status: "running", checkpoint: { phase: "summarize", tail } },
    }),
  });

  it("uses the durable task's numeric conversation and checkpoint tail", () => {
    expect(readCompactionIdentity(sql([row(1, 12)]), 9))
      .toEqual({ conversation: "1", task: "9", cutoff: "12" });
  });

  it("refuses malformed or inconsistent durable task identities", () => {
    for (const invalid of ["not-a-tail", "12", 0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, null]) {
      expect(readCompactionIdentity(sql([row(1, invalid)]), 9)).toBeNull();
      expect(readCompactionIdentity(sql([row(invalid, 12)]), 9)).toBeNull();
    }
    expect(readCompactionIdentity(sql([row(1, 12, 10)]), 9)).toBeNull();
    expect(readCompactionIdentity(sql([row(1, 12, 9, 2)]), 9)).toBeNull();
  });
});
