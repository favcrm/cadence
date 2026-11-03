import type { HostedState, MasterStatus, PendingOperation } from "./contract.js";
/** Application-owned business identity for the thread header. */
export type MasterMetadata = Readonly<Record<string, unknown>>;
/** The header's turn facts, from what the engine has not settled. */
export declare function operationStatus(pending: readonly PendingOperation[]): Pick<HostedState, "running" | "message" | "queued">;
/**
 * The board's master payload, in one place.
 *
 * The HTTP header read and the `master` stream frame use this shape, so they
 * cannot drift. The runtime is hosted, so the identity is pinned by the
 * deployment and the turn/queue facts come from the conversation object.
 *
 * `metadata` is the host's current public projection (for example `identity`
 * and `actor_identity`). It may add business fields but never replaces a field
 * this runtime owns: the header, model, session, turn and queue state are the
 * runtime's authority and take precedence.
 */
export declare function masterState(model: string, status: MasterStatus, conversationId: string | null, metadata?: MasterMetadata): Record<string, unknown>;
