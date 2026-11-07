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

  it("closes a slow thread stream when non-cursor updates fill its queue", async () => {
    const fixture = watchFixture();
    let listener: Parameters<AgentEventStream["start"]>[0] | undefined;
    const response = sessionEventResponse(
      {
        ...fixture.watch,
        start: (callback) => {
          listener = callback;
        },
      },
      {
        snapshot: (emit) => {
          emit("event: master\ndata: {}\n\n");
        },
        events: async (_events, emit) => {
          emit(`event: master\ndata: ${JSON.stringify("x".repeat(160_000))}\n\n`);
        },
      },
      { overflow: "after-cursor" },
    );
    if (!listener) throw new Error("watch did not start");
    await listener([], {} as never);
    await listener([], {} as never);
    const reader = response.body!.getReader();
    try {
      await reader.read();
      await reader.read();
      await reader.read();
      let ended = false;
      void reader.closed.then(() => {
        ended = true;
      });
      await Promise.resolve();
      expect(ended).toBe(true);
    } finally {
      await reader.cancel();
    }
  });

  it("keeps a durable cursor after an oversized initial master frame", async () => {
    const fixture = watchFixture();
    const response = sessionEventResponse(
      fixture.watch,
      {
        snapshot: (emit) => {
          emit(`event: master\ndata: ${JSON.stringify("x".repeat(280_000))}\n\n`);
          emit('id: 7\nevent: entry\ndata: {"seq":7}\n\n');
        },
        events: async () => {},
      },
      { overflow: "after-cursor" },
    );
    expect(await response.text()).toContain('id: 7\nevent: entry\ndata: {"seq":7}\n\n');
  });

  it("continues the diagnostic stream after an oversized snapshot is read", async () => {
    const fixture = watchFixture();
    let listener: Parameters<AgentEventStream["start"]>[0] | undefined;
    const response = sessionEventResponse(
      {
        ...fixture.watch,
        start: (callback) => {
          listener = callback;
        },
      },
      {
        snapshot: (emit) => {
          emit(`data: ${JSON.stringify("x".repeat(280_000))}\n\n`);
        },
        events: async (_events, emit) => {
          emit('data: {"type":"run_start"}\n\n');
        },
      },
    );
    if (!listener) throw new Error("oversized snapshot closed the watch");
    const next = listener([], {} as never);
    const reader = response.body!.getReader();
    try {
      expect(new TextDecoder().decode((await reader.read()).value)).toContain("x".repeat(100));
      await next;
      expect(new TextDecoder().decode((await reader.read()).value)).toBe(
        'data: {"type":"run_start"}\n\n',
      );
    } finally {
      await reader.cancel();
    }
  });

  it("releases a paused diagnostic watch when the reader disconnects", async () => {
    const fixture = watchFixture();
    let listener: Parameters<AgentEventStream["start"]>[0] | undefined;
    const response = sessionEventResponse(
      {
        ...fixture.watch,
        start: (callback) => {
          listener = callback;
        },
      },
      {
        snapshot: (emit) => {
          emit(`data: ${JSON.stringify("x".repeat(280_000))}\n\n`);
        },
        events: async (_events, emit) => {
          emit('data: {"type":"run_start"}\n\n');
        },
      },
    );
    if (!listener) throw new Error("oversized snapshot closed the watch");
    const next = listener([], {} as never);
    await response.body!.cancel();
    await next;
    expect(fixture.stopped()).toBe(true);
  });
});
