import type { AgentEventStream } from "@earendil-works/pi-durable";
import { describe, expect, it } from "bun:test";
import { sessionEventResponse } from "../src/session-stream.ts";

function watchFixture() {
  const completion =
    Promise.withResolvers<Awaited<AgentEventStream["closed"]>>();
  let stopped = false;
  return {
    completion,
    stopped: () => stopped,
    watch: {
      start() {},
      closed: completion.promise,
      async stop() {
        stopped = true;
        const end = { reason: "cancelled" as const };
        completion.resolve(end);
        return end;
      },
    },
  };
}

const format = {
  snapshot: (emit: (frame: string) => void) => emit(": connected\n\n"),
  events: async () => {},
};

describe("session stream lifecycle", () => {
  it("ends the HTTP response when the engine retires the watch, allowing a fresh reconnect", async () => {
    const fixture = watchFixture();
    const reader = sessionEventResponse(
      fixture.watch,
      format,
    ).body!.getReader();
    expect(new TextDecoder().decode((await reader.read()).value)).toBe(
      ": connected\n\n",
    );
    fixture.completion.resolve({ reason: "session_closed" });
    expect((await reader.read()).done).toBe(true);
  });

  for (const reason of ["session_failed", "listener_error"] as const) {
    it(`fails the HTTP body when the engine resolves a ${reason} termination`, async () => {
      const fixture = watchFixture();
      const reader = sessionEventResponse(
        fixture.watch,
        format,
      ).body!.getReader();
      await reader.read();
      fixture.completion.resolve({
        reason,
        error: new Error("session storage unavailable"),
      });
      await expect(reader.read()).rejects.toThrow(
        "session storage unavailable",
      );
    });
  }

  it("stops the subscription when the browser cancels its response", async () => {
    const fixture = watchFixture();
    const reader = sessionEventResponse(
      fixture.watch,
      format,
    ).body!.getReader();
    await reader.read();
    await reader.cancel();
    expect(fixture.stopped()).toBe(true);
  });

  it("releases the watch if its initial snapshot cannot be serialized", async () => {
    const fixture = watchFixture();
    const response = sessionEventResponse(fixture.watch, {
      ...format,
      snapshot() {
        throw new Error("invalid snapshot");
      },
    });
    await expect(response.text()).rejects.toThrow("invalid snapshot");
    expect(fixture.stopped()).toBe(true);
  });
});
