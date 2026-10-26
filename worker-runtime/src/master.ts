import type { MasterStatus } from "./contract.ts";

/** Application-owned business identity for the thread header. */
export type MasterMetadata = Readonly<Record<string, unknown>>;

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
export function masterState(
  model: string,
  status: MasterStatus,
  conversationId: string | null,
  metadata?: MasterMetadata,
): Record<string, unknown> {
  return {
    ...metadata,
    alias: "master",
    // The board only requires an unrestricted string here; `cadence` names
    // this hosted runtime truthfully and never claims a native agent daemon.
    provider: "cadence",
    endpoint_kind: "durable_object",
    live: true,
    model,
    model_label: model,
    // The model is resolved by the platform gateway; `model` is the id it last
    // served (or the configured default alias). It exposes no effort or
    // context telemetry; null is "not configured", never a fabricated value.
    effort: null,
    context: null,
    session:
      conversationId === null
        ? null
        : {
            id: conversationId,
            messages: null,
            streaming: status.running,
            compacting: false,
          },
    commands: [],
    turn: status.running
      ? {
          state: "working",
          message: null,
          conversation: conversationId,
          app_origin: null,
          summary: null,
          since: null,
        }
      : null,
    queue: [],
    queued: status.queued,
  };
}
