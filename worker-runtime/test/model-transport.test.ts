import { describe, expect, it } from "bun:test";
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

  it("shortens old tool results without shortening an artifact being edited", async () => {
    const old = "Earlier report ".repeat(2_000);
    const current = `<html>${"Current document ".repeat(2_000)}</html>`;
    const input = {
      messages: [
        { role: "user", content: "Read the earlier report." },
        { role: "assistant", content: null, tool_calls: [{ id: "old-call" }] },
        { role: "tool", tool_call_id: "old-call", content: old },
        { role: "assistant", content: "The earlier report is ready." },
        { role: "user", content: "Edit the current document." },
        { role: "assistant", content: null, tool_calls: [{ id: "current-call" }] },
        { role: "tool", tool_call_id: "current-call", content: current },
      ],
    };
    let received: Record<string, unknown> | undefined;
    const binding = meteredBinding(
      {
        async requestModel(request) {
          received = request;
          return new Response("ok");
        },
      },
      () => identity,
    );
    await binding.run("default", input);

    const messages = received?.messages as typeof input.messages;
    const oldResult = messages.find((message) => message.tool_call_id === "old-call");
    const currentResult = messages.find((message) => message.tool_call_id === "current-call");
    expect(oldResult?.content?.length).toBeLessThan(old.length);
    expect(oldResult?.content).toContain("shortened");
    expect(currentResult?.content).toBe(current);
    expect(input.messages[2]?.content).toBe(old);
    expect(input.messages[6]?.content).toBe(current);
  });

  it("keeps tool results intact when no completed earlier turn proves the boundary", async () => {
    const content = `<html>${"Current document ".repeat(2_000)}</html>`;
    const input = {
      messages: [
        { role: "user", content: "Edit the document." },
        { role: "assistant", content: null, tool_calls: [{ id: "current-call" }] },
        { role: "tool", tool_call_id: "current-call", content },
      ],
    };
    let received: Record<string, unknown> | undefined;
    const binding = meteredBinding(
      {
        async requestModel(request) {
          received = request;
          return new Response("ok");
        },
      },
      () => identity,
    );
    await binding.run("default", input);
    expect((received?.messages as typeof input.messages)[2]?.content).toBe(content);
  });

  it("splits an attested summary without losing text or breaking a surrogate pair", async () => {
    const source = `${"a".repeat(63_999)}😀${"z".repeat(100)}`;
    const input = {
      messages: [
        { role: "system", content: "Summarize." },
        { role: "user", content: [{ type: "text", text: source }] },
      ],
    };
    let received: Record<string, unknown> | undefined;
    const binding = meteredBinding(
      { async requestModel(request) { received = request; return new Response("ok"); } },
      () => identity,
      undefined,
      1,
      undefined,
      () => true,
    );
    await binding.run("default", input);
    const parts = (received?.messages as Array<{ role: string; content: string }>)
      .filter((message) => message.role === "user")
      .map((message) => message.content);
    expect(parts.every((part) => part.length <= 64_000)).toBe(true);
    expect(parts[0]).toBe("a".repeat(63_999));
    expect(parts.join("")).toBe(source);
    expect(input.messages[1]?.content).toEqual([{ type: "text", text: source }]);
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

  it("keeps private gateway diagnostics out of the budget refusal", async () => {
    const marker = "hosted_context_budget_0123456789abcdef0123456789abcdef";
    const binding = meteredBinding(
      {
        async requestModel() {
          return refusal(CONTEXT_BUDGET_EXCEEDED);
        },
      },
      () => identity,
      undefined,
      1,
      {
        clear: () => {},
        issue: () => marker,
      },
    );
    const response = (await binding.run(
      "default",
      { messages: [] },
      { returnRawResponse: true },
    )) as Response;
    expect(response.status).toBe(400);
    const body = (await response.json()) as GatewayRefusalBody;
    expect(JSON.stringify(body)).not.toContain("gw.internal");
    expect(JSON.stringify(body)).not.toContain("273589");
  });

  it("never records budget proof for untrusted HTTP or streamed error text", async () => {
    const responses = [
      refusal(CONTEXT_BUDGET_EXCEEDED, 413),
      refusal(CONTEXT_BUDGET_EXCEEDED, 400, "server_error"),
      Response.json(
        { error: { code: "invalid_request", message: CONTEXT_BUDGET_EXCEEDED_TEXT } },
        { status: 400 },
      ),
      new Response(
        `data: ${JSON.stringify({ error: { message: CONTEXT_BUDGET_EXCEEDED_TEXT } })}\n\n`,
        { headers: { "content-type": "text/event-stream" } },
      ),
      refusal("context_length_exceeded"),
    ];
    let issued = 0;
    for (const upstream of responses) {
      const binding = meteredBinding(
        { requestModel: async () => upstream },
        () => identity,
        undefined,
        1,
        {
          clear: () => {},
          issue: () => {
            issued++;
            return "unexpected";
          },
        },
      );
      await binding.run("default", {}, { returnRawResponse: true });
    }
    expect(issued).toBe(0);
  });
});
