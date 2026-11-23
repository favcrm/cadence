import { authorizeTool, authorizeTools } from "./tool-policy.ts";
import type { ToolAuthority } from "./tool-policy.ts";

/**
 * `@cadence/hosted-runtime/testing`: the real guards an application test needs
 * and opaque record fixtures for the engine's stored and live shapes.
 *
 * This entry imports no engine SDK and no Workers platform module. Record
 * builders return `unknown`, so consumers do not handle private record types.
 */

export { authorizeTool, authorizeTools };
export type { ToolAuthority };

/** The assistant parts a fixture record may carry. */
export type AssistantFixtureContent = readonly (
  | { type: "text"; text: string }
  | { type: "thinking"; thinking: string; redacted?: boolean }
)[];

/** One stored operator input, as the engine persists it. */
export function userRecord(id: number, text: string): unknown {
  return {
    id,
    conversationId: 1,
    kind: "pi.user",
    model: [{ role: "user", content: text, timestamp: 1000 }],
  };
}

/** One stored completed assistant turn, as the engine persists it. */
export function assistantRecord(
  id: number,
  content: AssistantFixtureContent,
): unknown {
  return {
    id,
    conversationId: 1,
    kind: "pi.assistant",
    model: [
      {
        role: "assistant",
        api: "openai-completions",
        provider: "openai",
        model: "synthetic",
        timestamp: 1000,
        stopReason: "stop",
        usage: {
          input: 0,
          output: 0,
          cacheRead: 0,
          cacheWrite: 0,
          totalTokens: 0,
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
        },
        content,
      },
    ],
  };
}

type FixtureView = {
  readonly entries: readonly unknown[];
  readonly docs: { readonly "pi.live": Record<string, never> };
};
type FixtureChange = {
  readonly kind: "value";
  readonly value: FixtureView;
  readonly ops: readonly [readonly ["r", FixtureView]];
  readonly retired: false;
};
type FixtureObserver = { advance(change: FixtureChange): void };
type FixtureEnd =
  | { readonly kind: "ended" }
  | { readonly kind: "listener_error"; readonly error: unknown };

/**
 * A finite engine-shaped conversation for host integration tests. Records and
 * watch views remain opaque to the host, and this fixture never filters them.
 */
export function conversationFixture(
  stored: () => readonly unknown[],
  live: () => unknown | undefined,
) {
  let committed = false;
  const observers = new Set<FixtureObserver>();
  const messages = async () => {
    const entry = committed ? live() : undefined;
    return entry === undefined ? stored() : [...stored(), entry];
  };
  const view = async (): Promise<FixtureView> => ({
    entries: await messages(),
    docs: { "pi.live": {} },
  });
  const session = {
    id: "1",
    messages,
    busy: async () => false,
    watch: async () => {
      let value = await view();
      const closed = Promise.withResolvers<FixtureEnd>();
      let ended = false;
      const finish = (end: FixtureEnd) => {
        if (!ended) {
          ended = true;
          closed.resolve(end);
        }
      };
      return {
        get value() { return value; },
        closed: closed.promise,
        start(listener: (change: FixtureChange) => Promise<void>) {
          const entry = live();
          if (entry === undefined) {
            finish({ kind: "ended" });
            return;
          }
          committed = true;
          value = { entries: [...stored(), entry], docs: { "pi.live": {} } };
          const change: FixtureChange = {
            kind: "value", value, ops: [["r", value]], retired: false,
          };
          for (const observer of observers) observer.advance(change);
          void listener(change).then(
            () => finish({ kind: "ended" }),
            (error: unknown) => finish({ kind: "listener_error", error }),
          );
        },
        async stop() {
          const end = { kind: "ended" } as const;
          finish(end);
          return end;
        },
      };
    },
  };
  return {
    session: () => session,
    conversation: async () => ({ entries: async () => ({ items: await messages() }) }),
    pending: async () => [],
    pi: async () => ({
      observeConversation: async (_id: unknown, observer: FixtureObserver) => {
        const value = await view();
        observers.add(observer);
        return { value, detach: () => { observers.delete(observer); } };
      },
    }),
  };
}
