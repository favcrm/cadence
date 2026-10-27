import { describe, expect, it } from "bun:test";
import { masterState } from "../src/master.ts";

/**
 * The master payload both the HTTP header and the stream frame use. The
 * runtime's own fields are the authority; a host projection only adds business
 * data.
 */
describe("master payload", () => {
  it("never lets metadata replace a runtime field", () => {
    const metadata: Record<string, unknown> = {
      model: "forged",
      model_label: "forged",
      provider: "pi",
      alias: "other",
      live: false,
      endpoint_kind: "daemon",
      effort: 3,
      context: 5,
      session: { id: "other" },
      turn: { state: "working" },
      queue: ["x"],
      queued: 99,
      commands: ["x"],
      identity: { look: "guard" },
    };
    const state = masterState(
      "alias",
      { running: true, queued: 0 },
      "conv-1",
      metadata,
    );
    expect(state.model).toBe("alias");
    expect(state.model_label).toBe("alias");
    expect(state.provider).toBe("cadence");
    expect(state.alias).toBe("master");
    expect(state.live).toBe(true);
    expect(state.endpoint_kind).toBe("durable_object");
    expect(state.effort).toBeNull();
    expect(state.context).toBeNull();
    expect(state.session).toEqual({
      id: "conv-1",
      messages: null,
      streaming: true,
      compacting: false,
    });
    expect(state.turn).toEqual({
      state: "working",
      message: null,
      conversation: "conv-1",
      app_origin: null,
      summary: null,
      since: null,
    });
    expect(state.queued).toBe(0);
    expect(state.queue).toEqual([]);
    expect(state.commands).toEqual([]);
    // Business identity survives without overriding the runtime's authority.
    expect(state.identity).toEqual({ look: "guard" });
  });
  it("refuses root serialization hooks without rejecting ordinary business data", () => {
    expect(() =>
      masterState("alias", { running: false, queued: 0 }, null, {
        toJSON: () => ({ provider: "forged", model: "forged" }),
      }),
    ).toThrow(TypeError);
    const state = JSON.parse(
      JSON.stringify(
        masterState("alias", { running: false, queued: 0 }, null, {
          toJSON: "ordinary business data",
        }),
      ),
    );
    expect(state.toJSON).toBe("ordinary business data");
    expect(state.provider).toBe("cadence");
    expect(state.model).toBe("alias");
  });
});
