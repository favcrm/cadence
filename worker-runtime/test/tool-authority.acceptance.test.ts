import { describe, expect, it } from "bun:test";
import type { RuntimeTool, ToolInvocation } from "../src/contract.ts";
import { authorizeTools, type ToolAuthority } from "../src/tool-policy.ts";

const invocation: ToolInvocation = {
  conversationId: 1,
  taskId: 2,
  now: () => 1000,
};

// Independently authored acceptance of the execution boundary, not SDK wiring.
// Effects are observed after refusals; the fake authority never filters them.
function guarded(execute: () => void, authority: ToolAuthority): RuntimeTool {
  return authorizeTools(
    {
      name: "synthetic-effects",
      tools: [
        {
          name: "synthetic_operation",
          description: "Record a synthetic operation.",
          parameters: {
            type: "object",
            properties: {},
            additionalProperties: false,
          },
          replay: "safe",
          async execute() {
            execute();
            return { output: [{ type: "text", text: "recorded" }] };
          },
        },
      ],
    },
    authority,
  ).tools![0]!;
}

describe("hosted tool-authority acceptance", () => {
  it("revalidates authority on replay after an actor is revoked", async () => {
    let permitted = true;
    let effects = 0;
    const tool = guarded(() => effects++, {
      toolRefusal: () => undefined,
      async authorizeTool() {
        if (!permitted) throw new Error("session_revoked");
      },
    });
    await tool.execute({}, invocation, {});
    permitted = false;
    await expect(tool.execute({}, invocation, {})).rejects.toThrow(
      "session_revoked",
    );
    expect(effects).toBe(1);
  });

  it("cannot execute a refused effect even for an authorized actor", async () => {
    let effects = 0;
    let authorizations = 0;
    const tool = guarded(() => effects++, {
      toolRefusal: () => "tool_effect_refused",
      async authorizeTool() {
        authorizations++;
      },
    });
    await expect(tool.execute({}, invocation, {})).rejects.toThrow(
      "tool_effect_refused",
    );
    expect(effects).toBe(0);
    expect(authorizations).toBe(0);
  });

  it("does not ask authority or execute an already-cancelled operation", async () => {
    let effects = 0;
    let authorizations = 0;
    const controller = new AbortController();
    controller.abort(new Error("operation_cancelled"));
    const tool = guarded(() => effects++, {
      toolRefusal: () => undefined,
      async authorizeTool() {
        authorizations++;
      },
    });
    await expect(
      tool.execute({}, invocation, { abortSignal: controller.signal }),
    ).rejects.toThrow("operation_cancelled");
    expect(effects).toBe(0);
    expect(authorizations).toBe(0);
  });

  it("cannot execute when cancellation arrives during authority validation", async () => {
    let effects = 0;
    const controller = new AbortController();
    const tool = guarded(() => effects++, {
      toolRefusal: () => undefined,
      async authorizeTool() {
        await Promise.resolve();
        controller.abort(new Error("operation_cancelled"));
      },
    });
    await expect(
      tool.execute({}, invocation, { abortSignal: controller.signal }),
    ).rejects.toThrow("operation_cancelled");
    expect(effects).toBe(0);
  });
});
