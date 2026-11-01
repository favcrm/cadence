import type { AgentEvent, AgentEventStream } from "@earendil-works/pi-durable";

type Emit = (frame: string) => void;

/**
 * The owner-only diagnostic stream carries engine snapshots and events, not
 * the board projection. Keep the image marker but never serialize image bytes
 * into that browser-facing stream.
 */
export function rawSessionFrame(event: unknown): string {
  const json = JSON.stringify(event, (_key, value: unknown) => {
    if (value === null || typeof value !== "object" || Array.isArray(value))
      return value;
    const part = value as Record<string, unknown>;
    if (
      part.type === "image" &&
      typeof part.data === "string" &&
      typeof part.mimeType === "string"
    ) {
      return { type: "image", mimeType: part.mimeType, redacted: true };
    }
    return value;
  });
  return `data: ${json}\n\n`;
}

/**
 * Both wire formats own the same watch lifecycle and cancellation rules.
 *
 * The returned body closes when the engine retires the watch (so a browser
 * reconnects to a fresh snapshot), fails when the watch reports a failure, and
 * stops the subscription when the client cancels.
 */
export function sessionEventResponse(
  watch: Pick<AgentEventStream, "start" | "stop" | "closed">,
  format: {
    snapshot: (emit: Emit) => void;
    events: (events: readonly AgentEvent[], emit: Emit) => Promise<void>;
  },
): Response {
  const encoder = new TextEncoder();
  let closed = false;
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      const emit: Emit = (frame) => {
        if (!closed) controller.enqueue(encoder.encode(frame));
      };
      const fail = (error: unknown) => {
        if (!closed) {
          closed = true;
          controller.error(error);
        }
      };
      // Observe termination before attaching the consumer, including a watch
      // that was already closed when its snapshot was obtained.
      void watch.closed.then((end) => {
        if (
          end.reason === "session_failed" ||
          end.reason === "listener_error"
        ) {
          fail(end.error);
          return;
        }
        if (!closed) {
          closed = true;
          controller.close();
        }
      }, fail);
      try {
        format.snapshot(emit);
        watch.start(async (events) => {
          if (!closed) await format.events(events, emit);
        });
      } catch (error) {
        fail(error);
        void watch.stop();
      }
    },
    async cancel() {
      closed = true;
      await watch.stop();
    },
  });
  return new Response(body, {
    headers: {
      "content-type": "text/event-stream",
      "cache-control": "no-store",
    },
  });
}
