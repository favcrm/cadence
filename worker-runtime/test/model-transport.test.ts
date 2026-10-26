import { describe, expect, it } from "bun:test";
import type { GenerationIdentity } from "../src/contract.ts";
import { meteredBinding } from "../src/model.ts";

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
