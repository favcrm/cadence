import type {
  RuntimeTool,
  RuntimeTools,
  ToolContext,
  ToolInvocation,
  ToolResult,
} from "./contract.ts";

/** The application-owned policy the runtime consults for every call. */
export interface ToolAuthority {
  /** The application's refusal for this tool name, or `undefined`. */
  toolRefusal(name: string): string | undefined;
  /** Revalidate the live authority of this conversation. */
  authorizeTool(name: string): Promise<void>;
}

/**
 * The one execution order: refusal, abort, live authorization, abort, then the
 * tool itself. A refusal or an aborted signal never reaches authorization or
 * the tool, and authorization runs again on every attempt, including a replay
 * whose intent is already durable.
 */
export async function withToolAuthority<T>(
  name: string,
  authority: ToolAuthority,
  context: ToolContext,
  execute: () => Promise<T>,
): Promise<T> {
  const refusal = authority.toolRefusal(name);
  if (refusal) throw new Error(refusal);
  context.abortSignal?.throwIfAborted();
  await authority.authorizeTool(name);
  context.abortSignal?.throwIfAborted();
  return execute();
}

/** Wrap one declared tool with the execution order above. */
export function authorizeTool(
  tool: RuntimeTool,
  authority: ToolAuthority,
): RuntimeTool {
  return {
    ...tool,
    execute(
      input: unknown,
      invocation: ToolInvocation,
      context: ToolContext,
    ): Promise<ToolResult> {
      return withToolAuthority(tool.name, authority, context, () =>
        tool.execute(input, invocation, context),
      );
    },
  };
}

/** Wrap one named group; the group's name and order are preserved. */
export function authorizeTools(
  group: RuntimeTools,
  authority: ToolAuthority,
): RuntimeTools {
  if (!group.tools) return group;
  return {
    ...group,
    tools: group.tools.map((tool) => authorizeTool(tool, authority)),
  };
}
