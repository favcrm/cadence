import { Database } from "bun:sqlite";
import { describe, expect, it } from "bun:test";
import {
  IdentitySnapshots,
  type IdentitySnapshotSql,
} from "../src/identity-snapshots.ts";

/**
 * The operation snapshot store over real SQLite: insert-once per opaque run
 * root, winner-observed concurrency, and fail-closed reads/writes. These are
 * the storage guarantees the generation hook depends on, not a reimplementation
 * of SQLite.
 */

function sqlStore(database: Database): IdentitySnapshotSql {
  return {
    exec<T = Record<string, unknown>>(
      query: string,
      ...bindings: (string | number | null)[]
    ) {
      if (/^\s*(SELECT|PRAGMA)/i.test(query)) {
        const rows = database.prepare(query).all(...bindings) as unknown as T[];
        return { toArray: () => rows };
      }
      database.prepare(query).run(...bindings);
      return { toArray: () => [] as T[] };
    },
  };
}

const rows = (database: Database, runId: string) =>
  database
    .prepare(
      "SELECT context, created_at FROM tenant_identity_snapshots WHERE run_id=?",
    )
    .all(runId) as Array<{ context: string; created_at: number }>;

describe("operation identity snapshots", () => {
  it("creates the preserved table and keeps an existing one with its rows", () => {
    const database = new Database(":memory:");
    const store = new IdentitySnapshots(sqlStore(database));
    expect(store).toBeDefined();
    const columns = database
      .prepare("PRAGMA table_info(tenant_identity_snapshots)")
      .all() as Array<{ name: string; notnull: number; pk: number }>;
    expect(columns.map((column) => column.name)).toEqual([
      "run_id",
      "context",
      "created_at",
    ]);
    expect(columns[0]!.pk).toBe(1);

    // An existing table and its rows survive a new store: CREATE IF NOT
    // EXISTS never resets, renames or drops the applied snapshot history.
    database
      .prepare(
        "INSERT INTO tenant_identity_snapshots (run_id,context,created_at) VALUES (?,?,?)",
      )
      .run("7", "stored before reopen", 11);
    const reopened = new IdentitySnapshots(sqlStore(database));
    expect(reopened).toBeDefined();
    expect(rows(database, "7")).toEqual([
      { context: "stored before reopen", created_at: 11 },
    ]);
  });

  it("stores one snapshot per run root and never reloads it", async () => {
    const database = new Database(":memory:");
    const store = new IdentitySnapshots(sqlStore(database));
    let loads = 0;
    const first = await store.readOrInsert(
      "4",
      async () => {
        loads += 1;
        return "identity one";
      },
      100,
    );
    expect(first).toBe("identity one");
    expect(rows(database, "4")).toEqual([
      { context: "identity one", created_at: 100 },
    ]);

    const second = await store.readOrInsert(
      "4",
      () => Promise.reject(new Error("a stored snapshot must not reload")),
      200,
    );
    expect(second).toBe("identity one");
    expect(loads).toBe(1);
    expect(rows(database, "4")).toHaveLength(1);
  });

  it("observes the insert-once winner when two operations race for one root", async () => {
    const database = new Database(":memory:");
    const store = new IdentitySnapshots(sqlStore(database));
    const [left, right] = await Promise.all([
      store.readOrInsert("9", async () => "context left", 1),
      store.readOrInsert("9", async () => "context right", 2),
    ]);
    // One writer wins; both callers observe that exact stored context.
    expect(left).toBe(right);
    expect(["context left", "context right"]).toContain(left);
    const stored = rows(database, "9");
    expect(stored).toHaveLength(1);
    expect(stored[0]!.context).toBe(left);
  });

  it("writes nothing when the canonical load refuses", async () => {
    const database = new Database(":memory:");
    const store = new IdentitySnapshots(sqlStore(database));
    await expect(
      store.readOrInsert(
        "5",
        () => Promise.reject(new Error("identity unavailable")),
        1,
      ),
    ).rejects.toThrow("identity unavailable");
    expect(rows(database, "5")).toHaveLength(0);
  });

  it("refuses an empty or oversized context without storing it", async () => {
    const database = new Database(":memory:");
    const store = new IdentitySnapshots(sqlStore(database));
    await expect(store.readOrInsert("1", async () => "", 1)).rejects.toThrow(
      /identity context is unavailable/i,
    );
    await expect(
      store.readOrInsert("2", async () => "x".repeat(64 * 1024 + 1), 1),
    ).rejects.toThrow(/identity context is unavailable/i);
    expect(
      database
        .prepare("SELECT COUNT(*) AS n FROM tenant_identity_snapshots")
        .get() as { n: number },
    ).toEqual({ n: 0 });
  });

  it("fails closed on a malformed stored snapshot", () => {
    const database = new Database(":memory:");
    const store = new IdentitySnapshots(sqlStore(database));
    database
      .prepare(
        "INSERT INTO tenant_identity_snapshots (run_id,context,created_at) VALUES (?,?,?)",
      )
      .run("3", "", 1);
    expect(() => store.readOrInsert("3", async () => "unused", 2)).toThrow(
      /identity snapshot is malformed/i,
    );
    database
      .prepare("UPDATE tenant_identity_snapshots SET context=? WHERE run_id=?")
      .run("x".repeat(64 * 1024 + 1), "3");
    expect(() => store.readOrInsert("3", async () => "unused", 2)).toThrow(
      /identity snapshot is malformed/i,
    );
  });
});
