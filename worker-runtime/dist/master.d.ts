import type { MasterStatus } from "./contract.js";
/**
 * The board's master payload, in one place.
 *
 * The HTTP header read and the `master` stream frame use this shape, so they
 * cannot drift. The runtime is hosted, so the identity is pinned by the
 * deployment and the turn/queue facts come from the conversation object.
 */
export declare function masterState(model: string, status: MasterStatus, conversationId: string | null): Record<string, unknown>;
