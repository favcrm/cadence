import {
  hook,
  ToolTask,
  type Extension,
  type ToolRegistration,
} from "@earendil-works/pi-durable";
import type { RuntimeTool, RuntimeTools, ToolContent } from "./contract.ts";
import type { ToolAuthority } from "./tool-policy.ts";
import { withToolAuthority } from "./tool-policy.ts";

/**
 * Private adaptation of the neutral contract onto the engine.
 *
 * Nothing here is exported from the package root: application code never sees
 * a registration, hook, prompt section or engine model.
 */
// Bounds apply to the total base64 payload of one tool result. The host's
// model gateway still enforces its own whole-request and image pricing limits.
const MAX_IMAGE_BASE64_CHARS = 192 * 1024;
const MAX_IMAGE_PARTS = 4;
const BASE64 = /^[A-Za-z0-9+/]+={0,2}$/;

function imageError(
  output: readonly ToolContent[],
  acceptsImages: boolean,
): string | undefined {
  let count = 0;
  let bytes = 0;
  for (const part of output) {
    if (part.type !== "image") continue;
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

function registration(
  tool: RuntimeTool,
  acceptsImages: boolean,
): ToolRegistration {
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
      const error = result.output && imageError(result.output, acceptsImages);
      if (error)
        return { output: [{ type: "text" as const, text: error }], isError: true };
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
    },
  };
}

/** One application group as an engine extension. */
export function runtimeExtension(
  group: RuntimeTools,
  acceptsImages = false,
): Extension {
  return {
    name: group.name,
    ...(group.tools
      ? { tools: group.tools.map((tool) => registration(tool, acceptsImages)) }
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
