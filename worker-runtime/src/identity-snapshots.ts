import { IDENTITY_CONTEXT_MAX } from "./assistant-identity.ts";

/**
 * Immutable identity context for one conversation operation, in the session
 * Durable Object's own SQLite.
 *
 * The engine hands run control to a new generation task after every tool round,
 * so a task memo is per round, not per operation. The run root
 * (`pi.live.run.inputs[0]`) is the operation's stable key across those
 * handovers, retries and recovery. Insert-once by that opaque root gives one
 * operation one canonical context; a new run root reads the newly committed
 * identity. This is an operation snapshot, never a profile owner or a
 * session-global cache.
 *
 * The table is created with `IF NOT EXISTS` under this runtime's ownership: an
 * existing table, its rows and its applied migration version are left as they
 * are.
 */

/** The narrow SQL surface this store needs; the platform's cursor exposes it. */
export interface IdentitySnapshotSql {
  exec<T = Record<string, unknown>>(
    query: string,
    ...bindings: (string | number | null)[]
  ): { toArray(): T[] };
}

/** The session's insert-once operation snapshot store. */
export interface IdentitySnapshotStore {
  readOrInsert(
    runId: string,
    load: () => Promise<string>,
    nowMs: number,
  ): Promise<string>;
}

const CREATE_TABLE = `CREATE TABLE IF NOT EXISTS tenant_identity_snapshots (
    run_id TEXT PRIMARY KEY,
    context TEXT NOT NULL,
    created_at INTEGER NOT NULL
  )`;

export class IdentitySnapshots implements IdentitySnapshotStore {
  constructor(private readonly sql: IdentitySnapshotSql) {
    this.sql.exec(CREATE_TABLE);
  }

  /**
   * The snapshot for `runId`, created exactly once from `load`. A refusal or
   * storage fault writes nothing; a malformed stored snapshot fails closed.
   */
  async readOrInsert(
    runId: string,
    load: () => Promise<string>,
    nowMs: number,
  ): Promise<string> {
    if (typeof runId !== "string" || runId.length === 0)
      throw new Error("The operation's identity root is unavailable.");
    const existing = this.row(runId);
    if (existing !== null) return existing;
    const text = await load();
    if (
      typeof text !== "string" ||
      text.length === 0 ||
      text.length > IDENTITY_CONTEXT_MAX
    )
      throw new Error("The operation's identity context is unavailable.");
    // Insert-once: a concurrent writer that won this root is observed below,
    // and both callers then return the winner's text.
    this.sql.exec(
      `INSERT OR IGNORE INTO tenant_identity_snapshots (run_id,context,created_at)
       VALUES (?,?,?)`,
      runId,
      text,
      nowMs,
    );
    const stored = this.row(runId);
    if (stored === null)
      throw new Error("The operation's identity snapshot is unavailable.");
    return stored;
  }

  private row(runId: string): string | null {
    const row = this.sql
      .exec<{ context: unknown }>(
        "SELECT context FROM tenant_identity_snapshots WHERE run_id=?",
        runId,
      )
      .toArray()[0];
    if (!row) return null;
    if (
      typeof row.context !== "string" ||
      row.context.length === 0 ||
      row.context.length > IDENTITY_CONTEXT_MAX
    )
      throw new Error("The operation's identity snapshot is malformed.");
    return row.context;
  }
}
