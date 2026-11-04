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
  ToolContent,
  ToolInvocation,
} from "./contract.ts";
import type { ToolAuthority } from "./tool-policy.ts";
import { withToolAuthority } from "./tool-policy.ts";
import { MAX_IMAGE_BASE64_CHARS, MAX_IMAGE_PARTS } from "./image-limits.ts";

/**
 * Private adaptation of the neutral contract onto the engine.
 *
 * Nothing here is exported from the package root: application code never sees
 * a registration, hook, prompt section or engine model.
 */
const BASE64 = /^[A-Za-z0-9+/]+={0,2}$/;

/**
 * Tool output is text or a validated image and nothing else: the engine would
 * otherwise carry any other part straight through as if it were an image.
 * A single result must fit within the whole-request image budget.
 */
function outputError(output: readonly ToolContent[], acceptsImages: boolean): string | undefined {
  let count = 0;
  let bytes = 0;
  for (const part of output) {
    if (part.type === "text" && typeof part.text === "string") continue;
    if (part.type !== "image") return "Tool output is invalid.";
    if (!acceptsImages) return "Image input is unavailable for this session.";
    count += 1;
    bytes += typeof part.data === "string" ? part.data.length : 0;
    if (
      count > MAX_IMAGE_PARTS ||
      bytes > MAX_IMAGE_BASE64_CHARS ||
      (part.mimeType !== "image/png" &&
        part.mimeType !== "image/jpeg" &&
        part.mimeType !== "image/webp") ||
      typeof part.data !== "string" ||
      part.data.length === 0 ||
      part.data.length % 4 !== 0 ||
      !BASE64.test(part.data)
    )
      return "Image output is invalid or too large.";
  }
  return undefined;
}

type ToolExecute = ToolRegistration["execute"];

/**
 * The app scope of the operation that owns one call, resolved by the session
 * from the engine's own run and placed entry. `null` means Home. A call that
 * cannot be attributed to an operation throws and is refused.
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
  acceptsImages: boolean,
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
        // Output is validated after the handler and before anything reaches
        // the model or the stored result.
        const error = result.output && outputError(result.output, acceptsImages);
        if (error)
          return {
            output: [{ type: "text" as const, text: error }],
            isError: true,
          };
        return {
          ...(result.output
            ? {
                output: result.output.map((part) =>
                  part.type === "text"
                    ? { type: "text" as const, text: part.text }
                    : {
                        type: "image" as const,
                        data: part.data,
                        mimeType: part.mimeType,
                      },
                ),
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
 * `acceptsImages` is the host's opt-in; without it an image result is refused.
 */
export function runtimeExtension(
  group: RuntimeTools,
  authority?: ToolAuthority,
  resolveApp?: AppScopeResolver,
  acceptsImages = false,
): Extension {
  return {
    name: group.name,
    ...(group.tools
      ? {
          tools: group.tools.map((tool) =>
            registration(tool, authority, resolveApp, acceptsImages),
          ),
        }
      : {}),
  };
}

function authorizeRegistration(tool: ToolRegistration, authority: ToolAuthority): ToolRegistration {
  return {
    ...tool,
    execute(input, api, context) {
      return withToolAuthority(tool.name, authority, { abortSignal: context.abortSignal }, () =>
        tool.execute(input, api, context),
      );
    },
  };
}

/**
 * Wrap an engine-owned extension's tools (the bundled skills' activation and
 * resource reads) with the same execution order an application tool gets.
 */
export function authorizeExtension(extension: Extension, authority: ToolAuthority): Extension {
  if (!extension.tools) return extension;
  return {
    ...extension,
    tools: extension.tools.map((tool) => authorizeRegistration(tool, authority)),
  };
}

/**
 * The declared-intent guard. It also covers nested calls, so a tool a program
 * invokes through `executeTool()` meets the application's refusal before it
 * runs; executions are reauthorized by their wrappers on recovery.
 */
export function intentGuard(refusal: (name: string) => string | undefined): Extension {
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
