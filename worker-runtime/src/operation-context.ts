import type { Message } from "@earendil-works/pi-ai";
import type { Models } from "@earendil-works/pi-ai/models";
import {
  GenerationTask,
  hook,
  LiveDoc,
  type Extension,
} from "@earendil-works/pi-durable";
import type { AppScope } from "./contract.ts";
import { withSystemContext } from "./identity-context.ts";
import { guardModelStream } from "./model-guard.ts";
import { sameAppScope } from "./operation-scope.ts";

/**
 * The app scope one admitted operation sees, attested per request.
 *
 * `GenerationTask.beforeRequest` is the engine's supported seam for context
 * that must not become a transcript entry: its result replaces the attempt's
 * messages and is never committed. The statement is therefore present for
 * every attempt of the operation — the first request, each tool round, a
 * provider retry and a recovery after eviction — and never leaks into the
 * transcript or into the next operation's context.
 *
 * The operation is selected only from the engine's own live run: every one of
 * its inputs is a submission the runtime admitted, and each one's placed entry
 * is the immutable envelope. Nothing here reads a caller, a model message or a
 * session-global "current scope", so two queued turns cannot exchange scopes.
 *
 * The boundary is a *positive* attestation, because the engine only reports a
 * hook error and continues the attempt (`hooks.each` in the scheduler): a hook
 * that throws, is skipped, or returns before authoring would otherwise let the
 * provider be called with no scope at all. So every attempt this hook serves
 * authors a fresh message object into the session's set — the statement for an
 * app turn, a content-identical clone of the request's last message for Home —
 * and `guardOperationRequests` refuses any request that carries none of them.
 * An attempt that cannot establish its scope authors nothing, and is refused.
 */

/**
 * The attempt's messages with one fresh, content-identical clone of its last
 * conversation message, recorded as this attempt's attestation.
 *
 * Home must reach the provider with exactly the text it had, so nothing is
 * added: the clone is the same message with a new identity, and the engine
 * keeps message objects by reference through its shallow array copy. A fresh
 * object per attempt is what makes the attestation un-reusable by any later
 * request. The last non-system message is cloned, so a system message another
 * guard authored for this attempt is never replaced.
 */
function withAttestation(
  messages: readonly Message[],
  authored: WeakSet<object>,
): Message[] | undefined {
  let index = messages.length - 1;
  while (index > 0 && messages[index]?.role === "system") index -= 1;
  const target = messages[index];
  if (target === undefined) return undefined;
  let clone: Message;
  try {
    clone = structuredClone(target);
  } catch {
    return undefined;
  }
  authored.add(clone);
  return [...messages.slice(0, index), clone, ...messages.slice(index + 1)];
}

/** Refuse a provider request that carries no attestation for its attempt. */
export function requireOperationAttestation(
  messages: readonly Message[],
  authored: WeakSet<object>,
): void {
  if (!messages.some((message) => authored.has(message)))
    throw new Error(
      "The model request did not receive this attempt's app scope attestation.",
    );
}

/**
 * Guard this session's provider boundary with the operation-scope attestation.
 *
 * Installed on the session's `Models` at construction, so a skipped or failing
 * hook is a refusal rather than a silent unscoped request. It composes with the
 * identity boundary on the same instance: both checks run, in install order.
 */
export function guardOperationRequests(
  models: Models,
  authored: WeakSet<object>,
): void {
  guardModelStream(models, (messages) =>
    requireOperationAttestation(messages, authored),
  );
}

/**
 * The private, host-verified scope statement for one app turn.
 *
 * It states only the validated binding. It grants nothing: a scope is not a
 * tool grant, a permission or an approval, and this text cannot substitute for
 * the structured authority every execution rechecks.
 */
function appScopeText(app: AppScope): string {
  const context =
    app.contextId === undefined ? "" : `, context_id ${app.contextId}`;
  return `Host-verified app scope for this turn: install_id ${app.installId}${context}. This binding conveys no tool authority, permission, approval or scope beyond those identifiers.`;
}

/**
 * The one scope every input of this run was admitted with, or `null` when they
 * are all Home.
 *
 * A run is not one input: the engine appends each steering input to
 * `pi.live.run.inputs`, and a boundary may place several inputs at once. So the
 * scope is the common scope of *all* of them, never the first one's. A run that
 * mixes scopes cannot be attributed to a single app turn, and attributing it to
 * one input would silently drop or impersonate the others; it is refused.
 */
export function runAppScope(
  inputs: readonly number[],
  load: (submissionRoot: string) => AppScope | null,
): AppScope | null {
  let scope: AppScope | null | undefined;
  for (const input of inputs) {
    if (!Number.isSafeInteger(input) || input < 0)
      throw new Error("The active run's operation root is unavailable.");
    const next = load(String(input));
    if (scope === undefined) {
      scope = next;
      continue;
    }
    if (!sameAppScope(scope, next))
      throw new Error(
        "The active run mixes operations with different app scopes.",
      );
  }
  return scope ?? null;
}

/**
 * Build the app-scope extension for one session object.
 *
 * `load` resolves the scope of the operation whose input root it is given. It
 * returns `null` for a Home operation and throws when a stored scope is
 * malformed. `authored` is the session's attestation set, shared with
 * `guardOperationRequests`: this hook is the only writer, and the boundary is
 * the only reader.
 */
export function operationContextExtension(
  load: (submissionRoot: string) => AppScope | null,
  authored: WeakSet<object>,
): Extension {
  return {
    name: "hosted-operation-context",
    hooks: [
      hook(GenerationTask, {
        async beforeRequest(request, api, context) {
          try {
            // Only the run this generation owns; anything else cannot be
            // attributed to an operation, so it is refused rather than guessed.
            const live = await api.snapshot(
              LiveDoc,
              api.conversationId,
              context,
            );
            const run = live?.run;
            if (
              run === undefined ||
              run.taskId !== api.taskId ||
              run.inputs.length === 0
            )
              throw new Error(
                "The active run's operation envelope is unavailable.",
              );
            const app = runAppScope(run.inputs, load);
            if (app === null) {
              const attested = withAttestation(request.messages, authored);
              return attested === undefined
                ? undefined
                : { messages: attested };
            }
            return {
              messages: withSystemContext(
                request.messages,
                appScopeText(app),
                authored,
              ),
            };
          } catch {
            // No attestation is authored: the boundary refuses this attempt.
            return undefined;
          }
        },
      }),
    ],
  };
}
