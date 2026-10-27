import { DurableObject } from "cloudflare:workers";
import type { AskResult, HostedSessionOptions, HostedState, PendingOperation, ThreadEntry } from "./contract.js";
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
    /** Ask the session and wait for the answer. */
    ask(prompt: string, operationId?: string): Promise<AskResult>;
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
    /** Submit without waiting: the board's send is queued, not a blocking turn. */
    submit(text: string, operationId: string): Promise<boolean>;
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
