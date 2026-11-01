import { createHash } from "node:crypto";
import { BOARD_MESSAGE_KIND, type AppScope } from "./contract.ts";
import { submissionHeld, type TaskSql } from "./identity.ts";

/**
 * The immutable envelope one operation was admitted with, in the session
 * Durable Object's own SQLite.
 *
 * The engine deduplicates a submission by its request id alone: a repeated id
 * returns the existing submission after comparing the submission *type*, never
 * its text or metadata. A retry therefore cannot rely on the engine to notice
 * that the same operation id now carries other text or another app scope; it
 * would silently reuse the first turn. This store is that check. The first
 * admission of an operation id records a digest of its text and its
 * host-verified app scope, an identical replay is the same operation, and any
 * change is refused before the engine is asked, so nothing is reused, rewritten
 * or impersonated.
 *
 * A refused admission writes nothing. The binding belongs to the session's
 * SQLite, not to one instance, so a rebuilt object keeps every operation's
 * envelope across queueing, retries and crash recovery.
 *
 * Before its first reservation the guard also asks the engine whether it
 * already holds that operation id (queued, placed or settled). An id the engine
 * knows but this guard never admitted was submitted by a build that predates
 * this table, so the engine — not the caller — is the only authority for what
 * that operation was. The guard therefore adopts it only as the Home envelope
 * the engine's own records prove: the exact text it stored, placed as the
 * board's message entry with no app scope. That envelope replays an identical
 * retry across the upgrade; a different text, any app scope, another kind or a
 * record that cannot be read is refused rather than re-bound, because the
 * engine would replay its own stored turn while the guard recorded another.
 * Nothing is ever adopted from the request being admitted.
 *
 * The table is created with `IF NOT EXISTS` under this runtime's ownership: an
 * existing table, its rows and any applied migration are left as they are.
 */

/** The narrow SQL surface this store needs; the platform's cursor exposes it. */
export type OperationScopeSql = TaskSql;

/** One operation's full envelope: its id, its text and its verified app scope. */
export interface OperationEnvelope {
  readonly operationId: string;
  readonly text: string;
  /** Absent for a Home operation. */
  readonly app?: AppScope;
}

const CREATE_TABLE = `CREATE TABLE IF NOT EXISTS tenant_operation_scopes (
    operation_id TEXT PRIMARY KEY,
    install_id TEXT,
    context_id TEXT,
    text_digest TEXT NOT NULL,
    created_at INTEGER NOT NULL
  )`;

/** Bounds that keep an identifier safe to store, echo and embed in a prompt. */
const OPERATION_ID_MAX = 200;
const OPERATION_TEXT_MAX = 1_000_000;
const CONTROL = /[\u0000-\u001f\u007f-\u009f]/;

/**
 * The board's binding grammar for one app part: 1–128 characters of
 * `[A-Za-z0-9_-]`. The transcript projects this same binding, so a retry can
 * send it back through the board unchanged.
 */
const APP_PART = /^[A-Za-z0-9_-]{1,128}$/;

/** Domain separation, so this digest can never equal one made for other data. */
const DIGEST_DOMAIN = "cadence.operation.envelope.v1\u0000";

/**
 * The digest one operation's text is bound by.
 *
 * The guard must recognise a changed text under a repeated operation id, but it
 * must not retain the text itself: the engine already stores the transcript,
 * and its own reset and compaction lifecycle governs that copy. A SHA-256
 * digest over a domain-separated, UTF-8 encoding keeps the exact-match check
 * (a changed text cannot collide in practice) while leaving nothing readable
 * behind in this table.
 */
function textDigest(text: string): string {
  return createHash("sha256")
    .update(DIGEST_DOMAIN, "utf8")
    .update(text, "utf8")
    .digest("hex");
}

const DIGEST = /^[0-9a-f]{64}$/;

function requirePart(value: unknown, label: string, pattern: RegExp): string {
  if (typeof value !== "string" || !pattern.test(value))
    throw new Error(`The operation's ${label} is malformed.`);
  return value;
}

function requireOperationId(value: unknown): string {
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    value.length > OPERATION_ID_MAX ||
    CONTROL.test(value)
  )
    throw new Error("The operation's operation id is malformed.");
  return value;
}

/**
 * One host-verified app scope, validated strictly: exactly `installId` and an
 * optional `contextId`, each in the board's binding grammar. Any other field is
 * refused, so a browser-supplied `authority`, `verified` or actor field can
 * never travel as if the host had verified it.
 */
export function parseAppScope(value: unknown): AppScope {
  if (typeof value !== "object" || value === null || Array.isArray(value))
    throw new Error("The operation's app scope is malformed.");
  if (
    Object.keys(value).some(
      (key) => key !== "installId" && key !== "contextId",
    )
  )
    throw new Error("The operation's app scope carries an unknown field.");
  const installId = requirePart(
    "installId" in value ? value.installId : undefined,
    "app installation",
    APP_PART,
  );
  const contextId = "contextId" in value ? value.contextId : undefined;
  // `null` is the canonical encoding of an absent optional context in a host's
  // own wire shape; it is the same installation-only binding, not a scope.
  if (contextId === undefined || contextId === null) return { installId };
  return {
    installId,
    contextId: requirePart(contextId, "app context", APP_PART),
  };
}

/**
 * The board's stored binding shape for one scope: the object a `thread_send`
 * retry sends back, and the object the daemon stamps on an operator entry.
 */
export function appPayload(app: AppScope): {
  install_id: string;
  context_id?: string;
} {
  return app.contextId === undefined
    ? { install_id: app.installId }
    : { install_id: app.installId, context_id: app.contextId };
}

/**
 * The app scope one stored entry carries, or `null` for a Home entry and for
 * any entry that is not an operator submission. A malformed stored scope
 * throws: a caller that acts on the scope must refuse rather than act outside
 * the envelope the operation was admitted with, and a reader that projects it
 * must fail rather than present the operation as Home.
 */
export function parseEntryAppScope(
  kind: unknown,
  data: unknown,
): AppScope | null {
  if (kind !== BOARD_MESSAGE_KIND) return null;
  if (typeof data !== "object" || data === null || Array.isArray(data))
    return null;
  if (!("app" in data) || data.app === undefined) return null;
  return parseAppScope(data.app);
}

/**
 * Whether two operation scopes are the same binding: both Home, or the same
 * installation with the same context. Never compares only one part, so a
 * promoted or demoted turn is a change.
 */
export function sameAppScope(
  left: AppScope | null,
  right: AppScope | null,
): boolean {
  return (
    (left?.installId ?? null) === (right?.installId ?? null) &&
    (left?.contextId ?? null) === (right?.contextId ?? null)
  );
}

/**
 * What the engine's own records prove about an operation id this guard never
 * admitted, as the session read them: the submission's type, the exact text it
 * was submitted with, and the entry metadata it was — or, for a queued input,
 * still would be — placed as.
 *
 * The session only reads. Whether that evidence proves an envelope is decided
 * here, so a caller cannot hand this store a verdict of its own, and the text
 * and metadata it carries come from the engine rather than from the request
 * being admitted.
 */
export interface EngineEvidence {
  /** The submission type the engine stored. */
  readonly type: unknown;
  /** The exact original text, or `null` when the engine no longer stores it. */
  readonly text: unknown;
  /** The stored entry's `kind`/`data`, or `null` when the engine stores none. */
  readonly entry: {
    readonly kind: unknown;
    readonly data: unknown;
  } | null;
}

/**
 * The Home envelope one legacy engine record proves, or `null` when it proves
 * nothing this guard may adopt.
 *
 * A build that predates this table admitted operations without one, so the
 * engine can hold a submission whose envelope was never recorded. What that
 * operation *was* is still in the engine's own records, and adopting it is safe
 * only for exactly that envelope: an input, placed as the board's own message
 * entry carrying this operation's id and no app scope, with the exact text the
 * engine stored. Every other shape — another submission type, another kind, an
 * entry that carries an app or any other field, a text the engine no longer
 * stores — proves nothing, and the caller refuses instead of adopting it.
 */
export function recoveredHomeEnvelope(
  operationId: string,
  evidence: EngineEvidence,
): OperationEnvelope | null {
  if (evidence.type !== "input") return null;
  const text = evidence.text;
  if (typeof text !== "string" || text.length > OPERATION_TEXT_MAX) return null;
  const entry = evidence.entry;
  if (entry === null || entry.kind !== BOARD_MESSAGE_KIND) return null;
  const data = entry.data;
  if (typeof data !== "object" || data === null || Array.isArray(data))
    return null;
  // Canonical no-app metadata is exactly one field, this operation's id. A
  // stored `app` is another binding, and an unknown field is a record this
  // guard cannot read, so neither is adopted as Home.
  const keys = Object.keys(data);
  if (keys.length !== 1 || keys[0] !== "messageId") return null;
  if (!("messageId" in data) || data.messageId !== operationId) return null;
  return { operationId, text };
}

/** The session's insert-once operation envelope store. */
export class OperationScopes {
  constructor(private readonly sql: OperationScopeSql) {
    this.sql.exec(CREATE_TABLE);
  }

  /**
   * Bind `operationId` to this envelope, or replay the envelope it already has.
   *
   * The first admission inserts the envelope's digest; an identical one returns
   * the admitted envelope; a changed text or app scope throws and leaves the
   * stored binding untouched. Validation happens before the write, so a refused
   * admission reserves nothing, and the text itself is never stored.
   *
   * An id this table has no row for may still be held by the engine, submitted
   * by a runtime build that predates this table. `evidence` is what the engine's
   * own records prove about it, read by the caller; `null` means it holds
   * nothing, and an omitted argument means the caller read nothing, in which
   * case the engine is asked here and any id it holds is refused rather than
   * re-bound. A proven legacy Home operation is adopted as the envelope the
   * engine stored, never as the caller's: a different text or any app scope is
   * refused, so an identical retry replays while a changed one cannot.
   */
  admit(
    envelope: OperationEnvelope,
    evidence?: EngineEvidence | null,
  ): OperationEnvelope {
    if (typeof envelope !== "object" || envelope === null)
      throw new Error("The operation's envelope is malformed.");
    const operationId = requireOperationId(envelope.operationId);
    const text = envelope.text;
    if (typeof text !== "string" || text.length > OPERATION_TEXT_MAX)
      throw new Error("The operation's text is malformed.");
    const digest = textDigest(text);
    const app = envelope.app === undefined ? null : parseAppScope(envelope.app);
    const admitted = this.row(operationId);
    if (admitted !== null)
      return this.replay(operationId, admitted, text, digest, app);
    if (evidence === undefined) {
      // The caller read nothing, so ask the engine here: an id it holds but
      // this guard never admitted has no envelope to compare with.
      if (submissionHeld(this.sql, operationId))
        throw new Error(
          `Operation "${operationId}" is already held by the engine without an admitted envelope.`,
        );
    } else if (evidence !== null) {
      return this.adopt(operationId, evidence, text, digest, app);
    }
    this.reserve(operationId, app, digest);
    return this.bind(operationId, text, digest, app);
  }

  /**
   * Adopt the envelope the engine's own records prove for an operation id this
   * guard never admitted, and bind it exactly as a first admission would.
   *
   * The recovered envelope is the authority in both directions: the caller's
   * text must be the stored text, and a caller that sends an app scope is
   * refused, because the engine's entry carries none and the turn would replay
   * as Home while the caller believed it scoped. The row written is the
   * recovered envelope, never the caller's, so from then on this guard and the
   * engine's replay agree. A refusal writes nothing.
   */
  private adopt(
    operationId: string,
    evidence: EngineEvidence,
    text: string,
    digest: string,
    app: AppScope | null,
  ): OperationEnvelope {
    const recovered = recoveredHomeEnvelope(operationId, evidence);
    if (recovered === null)
      throw new Error(
        `Operation "${operationId}" is already held by the engine without a provable Home envelope.`,
      );
    const recoveredDigest = textDigest(recovered.text);
    if (recoveredDigest !== digest)
      throw new Error(
        `Operation "${operationId}" is already held by the engine with a different text.`,
      );
    if (app !== null)
      throw new Error(
        `Operation "${operationId}" is already held by the engine as a Home operation and cannot be bound to an app scope.`,
      );
    this.reserve(operationId, null, recoveredDigest);
    return this.bind(operationId, text, digest, app);
  }

  /**
   * Record one envelope's digest and scope, insert-once: a row that already
   * exists is never rewritten, so every comparison reads the admitted envelope.
   */
  private reserve(
    operationId: string,
    app: AppScope | null,
    digest: string,
  ): void {
    this.sql.exec(
      `INSERT OR IGNORE INTO tenant_operation_scopes (operation_id,install_id,context_id,text_digest,created_at)
       VALUES (?,?,?,?,?)`,
      operationId,
      app === null ? null : app.installId,
      app === null || app.contextId === undefined ? null : app.contextId,
      digest,
      Date.now(),
    );
  }

  /** The row just reserved, compared with the envelope being admitted. */
  private bind(
    operationId: string,
    text: string,
    digest: string,
    app: AppScope | null,
  ): OperationEnvelope {
    const stored = this.row(operationId);
    if (stored === null)
      throw new Error("The operation's envelope is unavailable.");
    return this.replay(operationId, stored, text, digest, app);
  }

  /**
   * The admitted envelope, when it is exactly the one being admitted again.
   * The digest proves the text is the admitted text without this store keeping
   * it; the app scope is compared part by part.
   */
  private replay(
    operationId: string,
    stored: { digest: string; app: AppScope | null },
    text: string,
    digest: string,
    app: AppScope | null,
  ): OperationEnvelope {
    if (stored.digest !== digest || !sameAppScope(stored.app, app))
      throw new Error(
        `Operation "${operationId}" was already admitted with a different text or app scope.`,
      );
    return stored.app === null
      ? { operationId, text }
      : { operationId, text, app: stored.app };
  }

  /** The stored binding for `operationId`, or `null` when it was never admitted. */
  private row(
    operationId: string,
  ): { digest: string; app: AppScope | null } | null {
    const row = this.sql
      .exec(
        "SELECT install_id,context_id,text_digest FROM tenant_operation_scopes WHERE operation_id=?",
        operationId,
      )
      .toArray()[0];
    if (row === undefined) return null;
    const digest = row.text_digest;
    if (typeof digest !== "string" || !DIGEST.test(digest))
      throw new Error("The operation's stored envelope is malformed.");
    const installId = row.install_id;
    const contextId = row.context_id;
    if (installId === null && contextId === null) return { digest, app: null };
    if (typeof installId !== "string")
      throw new Error("The operation's stored envelope is malformed.");
    const app =
      contextId === null
        ? parseAppScope({ installId })
        : parseAppScope({ installId, contextId });
    return { digest, app };
  }
}
