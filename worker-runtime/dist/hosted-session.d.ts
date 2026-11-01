import { DurableObject } from "cloudflare:workers";
import type { AppScope, AskResult, HostedSessionOptions, HostedState, PendingOperation, ThreadEntry } from "./contract.js";
/**
 * One agent conversation, one Durable Object.
 *
 * The engine owns the loop: transcript, inbox, model calls, tools, retries and
 * crash recovery. It keeps all of it in this object's SQLite (`pi_*` tables)
 * and a Lifecycle job, so an evicted or redeployed object restarts from the
 * engine's last checkpoint and finishes the turn instead of losing it.
 *
 * The object name is the application's conversation key. The engine owns its
 * own session ids (the root is "1"), so the name is never treated as one.
 *
 * The application supplies capabilities and the authority policy; this base
 * supplies the prompt/skill profile, the model transport, the transcript
 * projection, the board wire behavior and the execution guards. Only declared
 * tools are installed, and every execution revalidates the live authority.
 */
export declare class HostedSession<Env extends object = Record<string, unknown>> extends DurableObject<Env> {
    private readonly models;
    private readonly registry;
    private readonly options;
    /**
     * The session's insert-once operation envelopes. The engine deduplicates by
     * request id alone, so this guard is what refuses a repeated id whose text
     * or app scope changed.
     */
    private readonly operationScopes;
    /** The app scope of the operation that owns one tool call, read from the engine. */
    private readonly toolAppScope;
    private readonly harness;
    /** The last model the gateway reported in this object's lifetime. */
    private resolvedModel;
    constructor(ctx: DurableObjectState, env: Env, options: HostedSessionOptions);
    /**
     * SSE for this conversation: the engine's snapshot, then one batch per
     * committed change. The stream lives in memory, so a client that reconnects
     * gets a fresh snapshot rather than a cursor.
     */
    fetch(request: Request): Promise<Response>;
    /**
     * Ask the session and wait for the answer.
     *
     * `app` is the host-verified scope of this turn; omit it for Home. The
     * operation id is bound to the text and scope it was first admitted with, so
     * a retry with the same id replays that operation and a retry that changes
     * either is refused before the engine runs.
     */
    ask(prompt: string, operationId?: string, app?: AppScope): Promise<AskResult>;
    /** Map the engine's committed records, including durable input metadata, for the board. */
    transcript(includeThinking: boolean): Promise<ThreadEntry[]>;
    /** Stored runtime records, not expanded presentation entries. Read-only. */
    recordCount(): Promise<number>;
    /** Abort this instance; durable state is retained when it is reconstructed. */
    reload(): string;
    /**
     * Start a fresh context for this conversation. The durable transcript before
     * this point stops being active; the board then has a clean thread even when
     * a stale session is wedged and a normal reload cannot recover it. The caller
     * is the owner-only reset route; this is deliberately not an automatic retry.
     */
    resetConversation(): Promise<void>;
    /**
     * Submit without waiting: the board's send is queued, not a blocking turn.
     *
     * `app` is the host-verified scope of this turn; omit it for Home. The
     * operation id is bound to the text and scope it was first admitted with, so
     * a retried send with the same id replays that operation and one that changes
     * either is refused before the engine is asked.
     */
    submit(text: string, operationId: string, app?: AppScope): Promise<boolean>;
    /**
     * Bind one operation to the exact text and app scope it was admitted with,
     * and return the stored envelope. A changed text or scope throws here, before
     * the engine sees the id, so a repeated id can never reuse another turn.
     *
     * An id this session never admitted may still be the engine's, from a build
     * that predates the guard, so the engine's own records are read first and
     * passed with the envelope: the guard adopts a legacy operation only as the
     * Home envelope those records prove, and refuses anything else.
     */
    private admitOperation;
    /**
     * What the engine's own records prove about an operation id this guard has no
     * envelope for: `null` when it holds nothing for it, otherwise the submission
     * type, the exact text and the entry metadata the guard needs to recognise a
     * legacy Home operation it may adopt.
     *
     * A placed input keeps its text and metadata in the entry it was placed as. A
     * queued one has no entry yet, so its draft is read from the engine's own
     * inbox document. That read awaits, and the engine may place the input and
     * drop the draft while it does — one commit, so the draft is gone exactly
     * when the submission has become placed. An absent draft is therefore re-read
     * once from the submission itself, never in a loop, and recovered from the
     * entry it now names; only a record that proves neither is left proving
     * nothing, and the guard refuses it. A submission that is not an input, and a
     * record the engine cannot produce, also yield evidence that proves nothing.
     * Reading a record the engine cannot parse throws, so an unreadable id is
     * refused rather than assumed free.
     */
    private engineEvidence;
    /**
     * The evidence one stored submission proves: its type, the exact text and the
     * entry metadata, when the engine placed an entry for it. A submission with
     * no entry proves no text here, which its caller refuses.
     */
    private placedEvidence;
    /**
     * The draft one queued input waits in the engine's inbox with, or `null` when
     * the inbox does not hold it. `content` is the exact text it was submitted
     * with and `entry` the metadata it will be placed as, so both are the
     * engine's own record of the operation rather than anything a caller sent.
     */
    private queuedInput;
    /**
     * The durable input entry an operation is placed as. The recorded message id
     * and the admitted app scope travel with the entry, so the transcript, a
     * rebuilt object and a replayed retry all read the same envelope.
     */
    private inputEntry;
    /**
     * The app scope of the operation whose input root is this engine submission,
     * read from the placed entry the engine stored. `null` is Home: the operation
     * was admitted without a scope, or the input is not an operation at all.
     */
    private operationAppScope;
    /** What the runtime has not settled yet, so the board can show queued/running/parked. */
    pendingOperations(): Promise<PendingOperation[]>;
    /**
     * Resolve and render the host's committed identity before admitting new
     * work. A refusal propagates unchanged, so an operation never starts when its
     * canonical identity is unavailable; a host without the feature is unchanged.
     */
    private requireIdentity;
    /**
     * The committed identity text this conversation's operations see. It is
     * resolved for admission and again for an operation's first request; only the
     * immutable operation snapshot stores it, so a change committed mid-operation
     * applies to the next one.
     */
    private identityContextText;
    /**
     * The one connection's public projection, checked before it is framed. A
     * malformed projection fails the connection exactly like an unavailable one,
     * so a partial header is never served.
     */
    private validatedThreadContext;
    /**
     * The operation-scoped identity of the model request running right now. It
     * comes from the engine's own running generation checkpoint (task id +
     * transcript cutoff), so provider retries and a crash recovery reuse the
     * same funding key while a tool round gets a new one. No running generation,
     * or more than one, is a refusal — never a guessed key.
     */
    private generationIdentity;
    /** Whether a run is in flight. */
    isBusy(): Promise<boolean>;
    /**
     * Cheap header state for `/api/master/state`: whether a turn is running, how
     * many inputs are queued, and the model label. `parked` operations are
     * neither running nor queued — the engine stopped them on its crash-loop
     * guard, so counting them as queued would invent work. The label is the
     * model the gateway actually served, or the configured alias until the first
     * call resolves the platform default.
     */
    state(): Promise<HostedState>;
    private recordResolvedModel;
    private modelLabel;
}
