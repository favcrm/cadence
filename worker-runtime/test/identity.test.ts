import { describe, expect, it } from "bun:test";
import {
  readCompactionFailure,
  readCompactionIdentity,
  readGenerationIdentity,
  readSubmissionSettlement,
  type TaskSql,
} from "../src/identity.ts";

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
  /** The compaction rows, the owning generation's status, and the newest entry. */
  const tasks = (
    rows: Record<string, unknown>[],
    ownerStatus = "terminal",
    latestEntryId = 0,
  ): TaskSql => ({
    exec(query) {
      return {
        toArray: () =>
          query.startsWith("SELECT status")
            ? [{ status: ownerStatus }]
            : query.startsWith("SELECT MAX")
              ? [{ latest: latestEntryId }]
              : rows,
      };
    },
  });
  const failed = (id: number, conversationId: number, outcome: unknown = { status: "failed" }) =>
    task(id, conversationId, outcome);

  it("reports a blocking failure once its owning generation settled", () => {
    expect(readCompactionFailure(tasks([failed(30, 1)]), 1)).toBe(true);
    expect(readCompactionFailure(tasks([failed(30, 1)], "running"), 1)).toBe(false);
    expect(readCompactionFailure(tasks([failed(30, 1, { status: "faulted" })]), 1)).toBe(true);
    expect(readCompactionFailure(tasks([failed(30, 1, { status: "orphaned" })]), 1)).toBe(true);
  });

  it("stops reporting once a later record owns the transcript", () => {
    expect(readCompactionFailure(tasks([failed(30, 1)], "terminal", 30), 1)).toBe(false);
    expect(readCompactionFailure(tasks([failed(30, 1)], "terminal", 12), 1)).toBe(true);
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

describe("submission settlement adapter", () => {
  /** One engine submission row, as the engine stores it. */
  const submission = (
    status: string,
    overrides: Record<string, unknown> = {},
    id = 21,
  ): Record<string, unknown> => ({
    id,
    conversation_id: 1,
    status,
    record: JSON.stringify({
      id,
      conversationId: 1,
      requestId: "op-a",
      type: "input",
      status,
      ...overrides,
    }),
  });

  /**
   * One session's durable state: the submission rows for the key, whether the
   * whole session object holds other queued or placed submissions, whether any
   * task is short of terminal, and which engine tables exist. The keyed read's
   * bindings are recorded so the JSON-encoded request id the engine indexes can
   * be asserted, and the work scans must bind nothing so they cover the whole
   * object.
   */
  const sessionState = (
    options: {
      rows?: readonly Record<string, unknown>[];
      outstanding?: boolean;
      working?: boolean;
      tables?: readonly string[];
      bindings?: unknown[];
    } = {},
  ): TaskSql => ({
    exec(query, ...bound) {
      if (/sqlite_master/.test(query)) {
        const name = String(bound[0]);
        const tables = options.tables ?? ["pi_submissions", "pi_tasks"];
        return {
          toArray: () =>
            tables
              .filter((table) => table === name)
              .map((table): Record<string, unknown> => ({ name: table })),
        };
      }
      if (/FROM pi_submissions\s+WHERE conversation_id=\? AND request_id=\?/.test(query)) {
        options.bindings?.push(...bound);
        return { toArray: () => [...(options.rows ?? [])] };
      }
      if (/FROM pi_submissions/.test(query)) {
        // The scan covers the whole session object, not one conversation.
        if (bound.length !== 0)
          throw new Error(`the work scan is conversation-scoped: ${query}`);
        return { toArray: () => (options.outstanding === true ? [{ held: 1 }] : []) };
      }
      if (/FROM pi_tasks/.test(query)) {
        if (bound.length !== 0)
          throw new Error(`the work scan is conversation-scoped: ${query}`);
        return { toArray: () => (options.working === true ? [{ held: 1 }] : []) };
      }
      throw new Error(`unexpected query: ${query}`);
    },
  });

  it("reads the one settled submission for the engine's exact key", () => {
    const bindings: unknown[] = [];
    expect(
      readSubmissionSettlement(
        sessionState({ rows: [submission("done")], bindings }),
        1,
        "op-a",
      ),
    ).toEqual({
      state: "settled",
      settled: { id: 21, conversation: 1, operationId: "op-a", status: "done" },
    });
    // The pair the engine deduplicates by: the conversation, and the request id
    // it stores JSON-encoded.
    expect(bindings).toEqual([1, '"op-a"']);

    expect(
      readSubmissionSettlement(sessionState({ rows: [submission("unanswered")] }), 1, "op-a"),
    ).toEqual({
      state: "settled",
      settled: {
        id: 21,
        conversation: 1,
        operationId: "op-a",
        status: "unanswered",
      },
    });
  });

  it("proves absence only while the session holds no outstanding work", () => {
    expect(readSubmissionSettlement(sessionState(), 1, "op-a")).toEqual({
      state: "absent",
    });
    // Another queued or placed submission anywhere in the session object, or a
    // task short of terminal, means the id could still be part of a session
    // that is mid-transition.
    expect(readSubmissionSettlement(sessionState({ outstanding: true }), 1, "op-a"))
      .toEqual({ state: "unknown" });
    expect(readSubmissionSettlement(sessionState({ working: true }), 1, "op-a"))
      .toEqual({ state: "unknown" });
    // A database without the engine's submissions table can hold none.
    expect(readSubmissionSettlement(sessionState({ tables: [] }), 1, "op-a"))
      .toEqual({ state: "absent" });
  });

  it("answers unknown for a partial engine schema", () => {
    // Tasks exist but the submissions table does not: no row can hold the id,
    // yet this is not the engine's schema and "at rest" cannot be read from it.
    expect(readSubmissionSettlement(sessionState({ tables: ["pi_tasks"] }), 1, "op-a"))
      .toEqual({ state: "unknown" });
    // Submissions exist, tasks do not: outstanding work cannot be ruled out,
    // with or without a record for the id.
    expect(
      readSubmissionSettlement(
        sessionState({ rows: [submission("done")], tables: ["pi_submissions"] }),
        1,
        "op-a",
      ),
    ).toEqual({ state: "unknown" });
    expect(
      readSubmissionSettlement(sessionState({ tables: ["pi_submissions"] }), 1, "op-a"),
    ).toEqual({ state: "unknown" });
  });

  it("answers unknown for a settled record while the session still has work", () => {
    // The id cannot be resubmitted, but the gate also requires nothing else in
    // this conversation to be queued, placed or running.
    expect(
      readSubmissionSettlement(
        sessionState({ rows: [submission("done")], outstanding: true }),
        1,
        "op-a",
      ),
    ).toEqual({ state: "unknown" });
    expect(
      readSubmissionSettlement(
        sessionState({ rows: [submission("done")], working: true }),
        1,
        "op-a",
      ),
    ).toEqual({ state: "unknown" });
  });

  it("answers unknown for work still held, or a record that disagrees", () => {
    for (const outstanding of ["queued", "placed"])
      expect(
        readSubmissionSettlement(
          sessionState({ rows: [submission(outstanding)] }),
          1,
          "op-a",
        ),
      ).toEqual({ state: "unknown" });
    const unknown = (rows: Record<string, unknown>[]) =>
      expect(readSubmissionSettlement(sessionState({ rows }), 1, "op-a"))
        .toEqual({ state: "unknown" });

    // Two rows for one deduplication key.
    unknown([submission("done"), submission("unanswered", {}, 22)]);
    // A record that is not a submission body, readable or not.
    unknown([{ id: 21, conversation_id: 1, record: "[]" }]);
    unknown([{ id: 21, conversation_id: 1, record: "{" }]);
    // An unknown lifecycle status.
    unknown([submission("later")]);
    // A body that disagrees with the question or the row it was read by.
    unknown([submission("done", { requestId: "op-b" })]);
    unknown([submission("done", { conversationId: 2 })]);
    unknown([{ ...submission("done"), conversation_id: 2 }]);
    // The status column its pending scans read disagrees with the record its
    // admission path returns.
    unknown([{ ...submission("done"), status: "queued" }]);
    // A submission that is not a prompt operation.
    unknown([submission("done", { type: "write" })]);
    // An unusable row id.
    unknown([submission("done", {}, 0)]);
  });

  it("throws on a malformed conversation or operation id", () => {
    expect(() => readSubmissionSettlement(sessionState(), 0, "op-a")).toThrow();
    expect(() => readSubmissionSettlement(sessionState(), 1, "")).toThrow();
  });
});
