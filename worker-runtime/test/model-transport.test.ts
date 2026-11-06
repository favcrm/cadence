import { describe, expect, it } from "bun:test";
import type { AssistantMessage } from "@earendil-works/pi-ai";
import { isContextOverflow } from "@earendil-works/pi-ai/utils/overflow";
import { isRetryableAssistantError } from "@earendil-works/pi-ai/utils/retry";
import type { GenerationIdentity } from "../src/contract.ts";
import {
  CONTEXT_BUDGET_EXCEEDED,
  CONTEXT_BUDGET_EXCEEDED_TEXT,
  meteredBinding,
} from "../src/model.ts";

const identity: GenerationIdentity = {
  conversation: "1",
  task: "2",
  cutoff: "3",
};

describe("model transport", () => {
  it("sends only the request input and generation identity, ignoring the model the session stores", async () => {
    const seen: {
      input: Record<string, unknown>;
      generation: GenerationIdentity;
    }[] = [];
    const binding = meteredBinding(
      {
        async requestModel(input, generation) {
          seen.push({ input, generation });
          return new Response("data: [DONE]\n\n", {
            headers: { "content-type": "text/event-stream" },
          });
        },
      },
      () => identity,
    );
    // A conversation that predates the platform default still stores the old pin.
    const response = (await binding.run(
      "@cf/zai-org/glm-5.3-flash",
      { messages: [] },
      { returnRawResponse: true },
    )) as Response;
    expect(response.status).toBe(200);
    expect(seen).toEqual([{ input: { messages: [] }, generation: identity }]);
  });

  it("refuses locally when the durable generation identity is unavailable", async () => {
    let called = false;
    const binding = meteredBinding(
      {
        async requestModel() {
          called = true;
          return new Response("unexpected");
        },
      },
      () => null,
    );
    const response = (await binding.run(
      "default",
      {},
      { returnRawResponse: true },
    )) as Response;
    expect(response.status).toBe(409);
    expect(called).toBe(false);
  });

  it("reports the model the gateway served and relays the same bytes", async () => {
    const served = "deepseek/deepseek-v4.1-flash-1017";
    const frames = [
      `data: ${JSON.stringify({ model: served, choices: [{ index: 0, delta: { content: "hi" } }] })}\n\n`,
      "data: [DONE]\n\n",
    ].join("");
    const resolved: string[] = [];
    const binding = meteredBinding(
      {
        requestModel: async () =>
          new Response(frames, {
            headers: { "content-type": "text/event-stream" },
          }),
      },
      () => identity,
      (model) => resolved.push(model),
    );
    const response = (await binding.run(
      "default",
      { messages: [] },
      {
        returnRawResponse: true,
      },
    )) as Response;
    expect(await response.text()).toBe(frames);
    expect(resolved).toEqual([served]);
  });

  it("scans a JSON body but never a refused response", async () => {
    const resolved: string[] = [];
    const refused = meteredBinding(
      {
        requestModel: async () =>
          Response.json({ model: "unserved-error" }, { status: 502 }),
      },
      () => identity,
      (model) => resolved.push(model),
    );
    const refusal = (await refused.run(
      "default",
      {},
      { returnRawResponse: true },
    )) as Response;
    expect(refusal.status).toBe(502);
    expect(await refusal.json()).toEqual({ model: "unserved-error" });

    const json = meteredBinding(
      { requestModel: async () => Response.json({ model: "served-json" }) },
      () => identity,
      (model) => resolved.push(model),
    );
    expect(await json.run("default", {}, { returnRawResponse: false })).toEqual(
      {
        model: "served-json",
      },
    );
    expect(resolved).toEqual(["served-json"]);
  });

  it("refuses an unsupported resume without dispatching a new model request", async () => {
    let dispatched = false;
    const binding = meteredBinding(
      {
        async requestModel() {
          dispatched = true;
          return new Response("unexpected dispatch");
        },
      },
      () => identity,
    );
    const refused = await binding.fetch("https://provider.example/stream");
    expect(refused.status).toBe(409);
    expect((await refused.json()).error.code).toBe("resume_unavailable");
    expect(dispatched).toBe(false);
  });
});

/** The gateway refusal bodies these cases observe, after the binding relays them. */
interface GatewayRefusalBody {
  readonly error: {
    readonly type?: string;
    readonly code?: string;
    readonly message?: string;
  };
}

describe("gateway refusals", () => {
  // The agentic gateway's own structured 400, with text the runtime must never
  // relay: a path and a byte count the board must not see.
  const refusal = (
    code: string,
    status = 400,
    type = "invalid_request_error",
  ): Response =>
    Response.json(
      {
        error: {
          type,
          code,
          message: "Private detail: 273589 bytes from https://gw.internal",
        },
      },
      { status },
    );

  it("names the trusted context-budget refusal as a terminal failure", async () => {
    let calls = 0;
    const binding = meteredBinding(
      {
        async requestModel() {
          calls += 1;
          return refusal(CONTEXT_BUDGET_EXCEEDED);
        },
      },
      () => identity,
    );
    const response = (await binding.run(
      "default",
      { messages: [] },
      { returnRawResponse: true },
    )) as Response;
    expect(response.status).toBe(400);
    const body = (await response.json()) as GatewayRefusalBody;
    expect(body).toEqual({
      error: {
        code: CONTEXT_BUDGET_EXCEEDED,
        message: CONTEXT_BUDGET_EXCEEDED_TEXT,
      },
    });
    expect(JSON.stringify(body)).not.toContain("gw.internal");
    expect(JSON.stringify(body)).not.toContain("273589");

    // The engine classifies the provider text the runtime emitted. Neither a
    // compaction nor a retry may follow, so no further call is dispatched.
    // The classifiers read only these fields of a terminal failure.
    const terminal = {
      stopReason: "error",
      errorMessage: body.error.message,
      provider: "openai",
    } as unknown as AssistantMessage;
    expect(isContextOverflow(terminal)).toBe(false);
    expect(isRetryableAssistantError(terminal)).toBe(false);
    expect(calls).toBe(1);
  });

  it("does not trust an incorrect status, type, or refusal code", async () => {
    const responses = [
      refusal(CONTEXT_BUDGET_EXCEEDED, 413),
      refusal(CONTEXT_BUDGET_EXCEEDED, 400, "server_error"),
      Response.json({ error: { code: CONTEXT_BUDGET_EXCEEDED } }, { status: 400 }),
      refusal("context_length_exceeded"),
      refusal("some_new_refusal"),
    ];
    for (const upstream of responses) {
      const binding = meteredBinding({ requestModel: async () => upstream }, () => identity);
      const response = (await binding.run("default", {}, { returnRawResponse: true })) as Response;
      const body = (await response.json()) as GatewayRefusalBody;
      expect(body.error.message).not.toBe(CONTEXT_BUDGET_EXCEEDED_TEXT);
    }
  });

});
