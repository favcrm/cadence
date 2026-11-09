import type { GenerationIdentity } from "./contract.ts";
import type { TaskSql } from "./identity.ts";

const MARKER = "hosted_context_budget_";
const OVERFLOW_SUFFIX = ":context_length_exceeded";

/** Durable evidence that a particular generation received the trusted refusal. */
export class ContextBudgetProofs {
  constructor(private readonly sql: TaskSql) {
    this.sql.exec(`CREATE TABLE IF NOT EXISTS tenant_context_budget_refusals (
      conversation TEXT NOT NULL,
      task TEXT NOT NULL,
      cutoff TEXT NOT NULL,
      nonce TEXT NOT NULL,
      PRIMARY KEY (conversation, task)
    )`);
  }

  /** A restarted or retried generation must not reuse evidence from an earlier call. */
  clear(identity: GenerationIdentity): void {
    this.sql.exec(
      "DELETE FROM tenant_context_budget_refusals WHERE conversation=? AND task=?",
      identity.conversation,
      identity.task,
    );
  }

  /** The nonce is emitted only after the gateway's structured refusal is verified. */
  issue(identity: GenerationIdentity): string {
    const nonce = crypto.randomUUID().replaceAll("-", "");
    this.sql.exec(
      `INSERT INTO tenant_context_budget_refusals (conversation,task,cutoff,nonce)
       VALUES (?,?,?,?)
       ON CONFLICT(conversation,task) DO UPDATE SET cutoff=excluded.cutoff, nonce=excluded.nonce`,
      identity.conversation,
      identity.task,
      identity.cutoff,
      nonce,
    );
    return `${MARKER}${nonce}${OVERFLOW_SUFFIX}`;
  }

  /** Check the committed engine entry's actual authoring task, not provider prose. */
  matches(
    entry: { conversationId?: unknown; byTaskId?: unknown },
    message: string | undefined,
  ): boolean {
    if (
      typeof message !== "string" ||
      !message.startsWith(MARKER) ||
      (typeof entry.conversationId !== "string" &&
        typeof entry.conversationId !== "number") ||
      (typeof entry.byTaskId !== "string" && typeof entry.byTaskId !== "number")
    )
      return false;
    const rows = this.sql
      .exec(
        "SELECT nonce FROM tenant_context_budget_refusals WHERE conversation=? AND task=?",
        String(entry.conversationId),
        String(entry.byTaskId),
      )
      .toArray();
    if (rows.length !== 1 || typeof rows[0]?.nonce !== "string") return false;
    const issued = `${MARKER}${rows[0].nonce}`;
    // Earlier deployed refusals used the same durable evidence without the
    // classifier suffix; they must remain readable in the stored transcript.
    return message === issued || message === `${issued}${OVERFLOW_SUFFIX}`;
  }

  /** A proven overflow is terminal only after its authoring task has ended. */
  settled(
    entry: { conversationId?: unknown; byTaskId?: unknown },
    message: string | undefined,
  ): boolean {
    if (!this.matches(entry, message)) return false;
    const conversation = Number(entry.conversationId);
    const task = Number(entry.byTaskId);
    if (!Number.isSafeInteger(conversation) || !Number.isSafeInteger(task)) return false;
    const rows = this.sql
      .exec(
        "SELECT status FROM pi_tasks WHERE id=? AND conversation_id=?",
        task,
        conversation,
      )
      .toArray();
    return rows.length === 1 && rows[0]?.status === "terminal";
  }
}
