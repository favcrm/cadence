import {
  hook,
  ToolTask,
  type Extension,
  type ToolRegistration,
} from "@earendil-works/pi-durable";
import type {
  AppScope,
  RuntimeTool,
  RuntimeTools,
  ToolInvocation,
} from "./contract.ts";
import type { ToolAuthority } from "./tool-policy.ts";
import { withToolAuthority } from "./tool-policy.ts";

/**
 * Private adaptation of the neutral contract onto the engine.
 *
 * Nothing here is exported from the package root: application code never sees
 * a registration, hook, prompt section or engine model.
 */

type ToolExecute = ToolRegistration["execute"];

/**
 * The app scope of the operation that owns one call, resolved by the session
 * from the engine's own run and placed entry. `null` means Home or a call that
 * runs outside an operation.
 */
export type AppScopeResolver = (
  api: Parameters<ToolExecute>[1],
  context: Parameters<ToolExecute>[2],
) => Promise<AppScope | null>;

/** The minimal per-call surface, including the operation's admitted app scope. */
function invocation(
  api: Parameters<ToolExecute>[1],
  app: AppScope | null,
): ToolInvocation {
  return {
    conversationId: api.conversationId,
    taskId: api.taskId,
    now: () => api.now(),
    ...(app === null ? {} : { app }),
  };
}

function registration(
  tool: RuntimeTool,
  authority: ToolAuthority | undefined,
  resolveApp: AppScopeResolver | undefined,
): ToolRegistration {
  return {
    name: tool.name,
    description: tool.description,
    parameters: tool.parameters as ToolRegistration["parameters"],
    ...(tool.replay === undefined ? {} : { replay: tool.replay }),
    execute(input, api, context) {
      const run = async () => {
        // The scope belongs to the operation, so it is read from the engine's
        // own run after the authority check and before the handler runs: a
        // refused or unauthorized call resolves nothing.
        const app =
          resolveApp === undefined ? null : await resolveApp(api, context);
        const result = await tool.execute(
          input,
          // Only the minimal invocation surface crosses the seam, never the
          // engine's task model, registry or nested-call authority.
          invocation(api, app),
          { abortSignal: context.abortSignal },
        );
        return {
          ...(result.output
            ? {
                output: result.output.map((part) => ({
                  type: "text" as const,
                  text: part.text,
                })),
              }
            : {}),
          ...(result.isError ? { isError: true } : {}),
        };
      };
      // A session always supplies its authority. A bare group installs as
      // given, which is how an application test proves a tool's own contract.
      if (authority === undefined) return run();
      return withToolAuthority(
        tool.name,
        authority,
        { abortSignal: context.abortSignal },
        run,
      );
    },
  };
}

/**
 * One application group as an engine extension.
 *
 * `authority` applies the session's refusal and live authorization to every
 * call; `resolveApp` supplies the operation's admitted app scope to the
 * handler's invocation. Both are optional so a group can be installed as-is.
 */
export function runtimeExtension(
  group: RuntimeTools,
  authority?: ToolAuthority,
  resolveApp?: AppScopeResolver,
): Extension {
  return {
    name: group.name,
    ...(group.tools
      ? {
          tools: group.tools.map((tool) =>
            registration(tool, authority, resolveApp),
          ),
        }
      : {}),
  };
}

function authorizeRegistration(
  tool: ToolRegistration,
  authority: ToolAuthority,
): ToolRegistration {
  return {
    ...tool,
    execute(input, api, context) {
      return withToolAuthority(
        tool.name,
        authority,
        { abortSignal: context.abortSignal },
        () => tool.execute(input, api, context),
      );
    },
  };
}

/**
 * Wrap an engine-owned extension's tools (the bundled skills' activation and
 * resource reads) with the same execution order an application tool gets.
 */
export function authorizeExtension(
  extension: Extension,
  authority: ToolAuthority,
): Extension {
  if (!extension.tools) return extension;
  return {
    ...extension,
    tools: extension.tools.map((tool) =>
      authorizeRegistration(tool, authority),
    ),
  };
}

/**
 * The declared-intent guard. It also covers nested calls, so a tool a program
 * invokes through `executeTool()` meets the application's refusal before it
 * runs; executions are reauthorized by their wrappers on recovery.
 */
export function intentGuard(
  refusal: (name: string) => string | undefined,
): Extension {
  return {
    name: "hosted-tool-policy",
    hooks: [
      hook(ToolTask, {
        beforeTool: (call) => {
          const block = refusal(call.name);
          return block ? { block } : undefined;
        },
      }),
    ],
  };
}
