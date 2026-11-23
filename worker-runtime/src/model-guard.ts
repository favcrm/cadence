import type { Message } from "@earendil-works/pi-ai";
import type { Models } from "@earendil-works/pi-ai/models";

/**
 * The guarded provider boundary for this session's `Models`.
 *
 * `streamSimple` is the call that actually reaches the provider. A guard is a
 * check over the messages an attempt is about to send; a failing check throws
 * before the provider is called, which fails the attempt and settles its
 * submissions unanswered.
 *
 * Guards compose: several runtimes may guard one `Models` instance (the
 * identity boundary and the operation-scope boundary do), so every installed
 * check runs in order and the original stream is called once. Compaction's
 * summarizer is the SDK's only `completeSimple` caller and is not a turn: it
 * builds its own inline messages, so it keeps the original path and meets no
 * turn guard.
 */

type Check = (messages: readonly Message[]) => void;

interface Boundary {
  readonly checks: Check[];
  readonly original: Models["streamSimple"];
}

const boundaries = new WeakMap<object, Boundary>();

/** Install one check on this `Models` instance's provider boundary. */
export function guardModelStream(models: Models, check: Check): void {
  const existing = boundaries.get(models);
  if (existing !== undefined) {
    existing.checks.push(check);
    return;
  }
  const original = models.streamSimple.bind(models);
  const checks: Check[] = [check];
  boundaries.set(models, { checks, original });
  models.streamSimple = (model, context, options) => {
    for (const guard of checks) guard(context.messages);
    return original(model, context, options);
  };
  models.completeSimple = (model, context, options) =>
    original(model, context, options).result();
}
