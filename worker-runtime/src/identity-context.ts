import type { Message } from "@earendil-works/pi-ai";
import type { Models } from "@earendil-works/pi-ai/models";
import {
  GenerationTask,
  hook,
  LiveDoc,
  type Extension,
} from "@earendil-works/pi-durable";
import type { IdentitySnapshotStore } from "./identity-snapshots.ts";

/**
 * The committed identity the model sees, injected per request.
 *
 * `GenerationTask.beforeRequest` is the engine's supported seam for context
 * that must not become a transcript entry: its result replaces the attempt's
 * messages and is never committed.
 *
 * The engine hands run control to a new generation task after every tool
 * round, so a task memo is per round, not per operation. The stable operation
 * root is `pi.live.run.inputs[0]`; the session's insert-once snapshot for that
 * opaque root is reused across handovers, retries and recovery. A new run root
 * reads the newly committed identity. The hook never takes a run or actor from
 * the model or a client, and never stores a session-global identity copy.
 *
 * Nothing here writes a user message, a prompt copy or a mutable global cache.
 */

/**
 * One system message after the leading prompts; the leading prompt stays
 * intact. The exact object is recorded in the session's WeakSet so the model
 * boundary can prove this request received it.
 */
function withSystemContext(
  messages: readonly Message[],
  text: string,
  authored: WeakSet<object>,
): Message[] {
  const system: Message = {
    role: "system",
    content: text,
    timestamp: Date.now(),
  };
  authored.add(system);
  let index = 0;
  while (index < messages.length && messages[index]!.role === "system")
    index += 1;
  return [...messages.slice(0, index), system, ...messages.slice(index)];
}

/**
 * Refuse a model request that does not carry one of the system message objects
 * this session authored for it. Checking the object identity — never a text
 * marker or current storage — proves the request actually received the
 * canonical context this operation snapshot holds. Compaction is handled
 * separately by `completeSimple`, whose request is an internal summarization,
 * not a turn.
 */
export function requireAuthoredIdentity(
  messages: readonly Message[],
  authored: WeakSet<object>,
): void {
  if (!messages.some((message) => authored.has(message)))
    throw new Error(
      "The model request did not receive the canonical assistant identity context.",
    );
}

/**
 * Guard the actual generation model call on this session's `Models` instance.
 *
 * The engine runs `beforeRequest`, then
 * `runtime.models.streamSimple(model, { messages: [...messages] }, options)`.
 * The shallow copy keeps the authored message object by reference, so checking
 * `context.messages` here proves this request received the operation
 * snapshot's identity before the SDK normalizes/serializes it.
 *
 * Compaction's summarizer is the only `completeSimple` caller in the SDK; its
 * final request builds its own inline system/user messages, so marking
 * `beforeCompact` messages cannot reach this boundary. Reuse the captured
 * original `streamSimple` for `completeSimple`: identical normalization, auth
 * and transport, no identity guard, no text marker, prompt copy, model or route
 * change. A deferred poll uses `fetchDeferred`, which only continues an
 * already-admitted call.
 */
export function guardModelRequests(
  models: Models,
  authored: WeakSet<object>,
): void {
  const unguardedStreamSimple = models.streamSimple.bind(models);
  models.streamSimple = (model, context, options) => {
    requireAuthoredIdentity(context.messages, authored);
    return unguardedStreamSimple(model, context, options);
  };
  models.completeSimple = (model, context, options) =>
    unguardedStreamSimple(model, context, options).result();
}

/**
 * Build the identity extension for one session object.
 *
 * `load` resolves the committed context for this conversation's scope and
 * actor. It throws when the host's canonical identity is unavailable; there is
 * no empty or generic fallback, so an operation that could not be scoped never
 * receives a substitute prompt.
 */
export function identityContextExtension(
  load: () => Promise<string>,
  authored: WeakSet<object>,
  snapshots: IdentitySnapshotStore,
): Extension {
  return {
    name: "hosted-identity-context",
    hooks: [
      hook(GenerationTask, {
        async beforeRequest(request, api, context) {
          // Select the operation only from the engine's own live run, and only
          // while this generation task is the run's current owner.
          const live = await api.snapshot(LiveDoc, api.conversationId, context);
          const run = live?.run;
          if (
            run === undefined ||
            run.taskId !== api.taskId ||
            run.inputs.length === 0
          )
            throw new Error(
              "The active run's identity snapshot is unavailable.",
            );
          const root = run.inputs[0];
          if (root === undefined || !Number.isSafeInteger(root) || root < 0)
            throw new Error("The active run's identity root is unavailable.");
          const text = await snapshots.readOrInsert(
            String(root),
            load,
            Date.now(),
          );
          return {
            messages: withSystemContext(request.messages, text, authored),
          };
        },
      }),
    ],
  };
}
