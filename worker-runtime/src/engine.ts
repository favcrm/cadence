import {
  hook,
  ToolTask,
  type Extension,
  type ToolRegistration,
} from "@earendil-works/pi-durable";
import type { RuntimeTool, RuntimeTools } from "./contract.ts";
import type { ToolAuthority } from "./tool-policy.ts";
import { withToolAuthority } from "./tool-policy.ts";

/**
 * Private adaptation of the neutral contract onto the engine.
 *
 * Nothing here is exported from the package root: application code never sees
 * a registration, hook, prompt section or engine model.
 */

function registration(tool: RuntimeTool): ToolRegistration {
  return {
    name: tool.name,
    description: tool.description,
    parameters: tool.parameters as ToolRegistration["parameters"],
    ...(tool.replay === undefined ? {} : { replay: tool.replay }),
    async execute(input, api, context) {
      const result = await tool.execute(
        input,
        // Only the minimal invocation surface crosses the seam, never the
        // engine's task model, registry or nested-call authority.
        {
          conversationId: api.conversationId,
          taskId: api.taskId,
          now: () => api.now(),
        },
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
    },
  };
}

/** One application group as an engine extension. */
export function runtimeExtension(group: RuntimeTools): Extension {
  return {
    name: group.name,
    ...(group.tools ? { tools: group.tools.map(registration) } : {}),
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
