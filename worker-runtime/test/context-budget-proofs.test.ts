import { Database } from "bun:sqlite";
import { describe, expect, it } from "bun:test";
import { ContextBudgetProofs } from "../src/context-budget-proofs.ts";
import type { TaskSql } from "../src/identity.ts";

function sqlStore(database: Database): TaskSql {
  return {
    exec<T = Record<string, unknown>>(
      query: string,
      ...bindings: (string | number | null)[]
    ) {
      if (/^\s*SELECT/i.test(query)) {
        const rows = database.prepare(query).all(...bindings) as unknown as T[];
        return { toArray: () => rows };
      }
      database.prepare(query).run(...bindings);
      return { toArray: () => [] as T[] };
    },
  };
}

const identity = { conversation: "session_1", task: "42", cutoff: "15" };
const entry = { conversationId: "session_1", byTaskId: 42 };

describe("context budget refusal evidence", () => {
  it("authenticates only the issued marker for its own durable task, including after reconstruction", () => {
    const db = new Database(":memory:");
    const proofs = new ContextBudgetProofs(sqlStore(db));
    const marker = proofs.issue(identity);
    expect(proofs.matches(entry, marker)).toBe(true);
    expect(new ContextBudgetProofs(sqlStore(db)).matches(entry, marker)).toBe(
      true,
    );
    // Refusals persisted by the previous release lack the classifier suffix.
    expect(proofs.matches(entry, marker.split(":")[0])).toBe(true);
    expect(proofs.matches({ ...entry, byTaskId: 43 }, marker)).toBe(false);
    expect(proofs.matches({ ...entry, conversationId: "other" }, marker)).toBe(
      false,
    );
    expect(
      proofs.matches(
        entry,
        "The context limit was reached. Start a fresh conversation or split the task and retry.",
      ),
    ).toBe(false);
    db.close();
  });

  it("invalidates a prior marker on redispatch and cannot mistake an older concurrent response", () => {
    const db = new Database(":memory:");
    const proofs = new ContextBudgetProofs(sqlStore(db));
    const first = proofs.issue(identity);
    const other = proofs.issue({ ...identity, task: "43" });
    proofs.clear(identity);
    expect(proofs.matches(entry, first)).toBe(false);
    expect(proofs.matches({ ...entry, byTaskId: 43 }, other)).toBe(true);
    const second = proofs.issue(identity);
    expect(second).not.toBe(first);
    expect(proofs.matches(entry, first)).toBe(false);
    expect(proofs.matches(entry, second)).toBe(true);
    db.close();
  });

  it("cannot issue evidence when durable storage is unavailable", () => {
    const db = new Database(":memory:");
    const proofs = new ContextBudgetProofs(sqlStore(db));
    db.close();
    expect(() => proofs.issue(identity)).toThrow();
  });
});
