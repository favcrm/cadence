import type { AgentEvent, AgentEventStream } from "@earendil-works/pi-durable";

/** `true` when the frame was accepted; `false` once this response is ending. */
export type Emit = (frame: string) => boolean;
export type PreviewEmit = (text: string | null) => boolean;

/**
 * How many bytes of not-yet-read SSE frames one response may queue.
 *
 * A slow browser must not make the stream grow without bound. When enqueuing a
 * frame takes the queue past this mark, the response closes gracefully: the
 * queue is still drained, so every committed entry already enqueued reaches
 * the client, and the browser reconnects with its last received `id:` as the
 * resume cursor. The retry re-reads the transcript after that cursor, so no
 * committed entry is lost and each connection still advances the cursor even
 * for a very large history. Preview frames have no `id:` and are coalesced, so
 * they never add to the durable backlog.
 */
const MAX_QUEUED_BYTES = 256 * 1024;

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
    if (part.type === "image" && typeof part.data === "string") {
      return {
        type: "image",
        ...(typeof part.mimeType === "string" ? { mimeType: part.mimeType } : {}),
        redacted: true,
      };
    }
    return value;
  });
  return `data: ${json}\n\n`;
}

/** One connection's projection of engine batches. */
export interface SessionStreamFormat {
  snapshot(emit: Emit, preview: PreviewEmit): void;
  events(
    events: readonly AgentEvent[],
    emit: Emit,
    preview: PreviewEmit,
  ): Promise<void>;
}

/** How a response decides it has queued enough. */
export interface SessionStreamOptions {
  /**
   * `"after-cursor"` lets an oversized snapshot preface send its first durable
   * entry before closing, so a reconnect can advance. Once the snapshot has
   * finished, later non-cursor frames also close a full queue: reconnecting
   * re-reads current state and resumes from the client's existing cursor.
   * The diagnostic stream waits for an oversized initial snapshot to drain
   * before delivering later events; it has no cursor to escape a reconnect loop.
   */
  readonly overflow?: "after-cursor" | "always";
}

/**
 * Both wire formats own the same watch lifecycle and cancellation rules.
 *
 * The returned body closes when the engine retires the watch (so a browser
 * reconnects to a fresh snapshot), fails when the watch reports a failure, and
 * stops the subscription when the client cancels. It also closes gracefully
 * once its queued bytes pass `MAX_QUEUED_BYTES`: a slow reader resumes from the
 * last durable `id:` it received instead of letting the response hold an
 * unbounded backlog. Because the queue is still drained, that close never
 * drops an enqueued entry. With `overflow: "after-cursor"` an oversized
 * snapshot preface still lets its first resumable entry through; after the
 * snapshot, a full queue closes even when no new entry has arrived.
 * An oversized diagnostic snapshot is allowed through once; its watch pauses
 * until the reader drains it. The engine bounds and coalesces pending batches
 * while that listener is paused.
 *
 * A preview frame is transient: only the newest one matters, so a slow reader
 * never accumulates a frame per streamed chunk. At most one preview waits
 * outside the stream at any time; a later preview replaces it, and the stream
 * flushes it before the next durable frame (or on `pull`). That keeps a
 * coalesced preview ahead of the entry it belongs to without a timer.
 */
export function sessionEventResponse(
  watch: Pick<AgentEventStream, "start" | "stop" | "closed">,
  format: SessionStreamFormat,
  options?: SessionStreamOptions,
): Response {
  const encoder = new TextEncoder();
  let closed = false;
  let sink: ReadableStreamDefaultController<Uint8Array> | null = null;
  /** The newest preview not yet enqueued; `null` means nothing is waiting. */
  let pending: { readonly text: string | null } | null = null;
  /** Set once a frame that carries a durable resume cursor was enqueued. */
  let cursor = false;
  /** Snapshot entries get a chance to advance the cursor before closing. */
  let snapshotComplete = false;
  let initialDrain: Promise<void> | null = null;
  let releaseInitialDrain: (() => void) | null = null;
  const resumeInitialDrain = (): void => {
    const release = releaseInitialDrain;
    releaseInitialDrain = null;
    initialDrain = null;
    release?.();
  };

  /** End this response after its queued bytes drain; the client reconnects
   *  with the last durable cursor it received. */
  const closeForBackpressure = (): void => {
    if (closed || sink === null) return;
    closed = true;
    pending = null;
    resumeInitialDrain();
    sink.close();
    void watch.stop();
  };

  const flushPreview = (): void => {
    if (closed || pending === null || sink === null) return;
    const { text } = pending;
    pending = null;
    // The transient in-flight text. No `id:` line: a preview never advances
    // the browser's durable cursor.
    sink.enqueue(
      encoder.encode(`event: preview\ndata: ${JSON.stringify({ text })}\n\n`),
    );
  };

  const emit: Emit = (frame) => {
    if (closed || sink === null) return false;
    // A waiting preview is newer than the last enqueued chunk and older than
    // this frame: place it first so a committed entry can never overtake the
    // in-flight text it replaces.
    flushPreview();
    if (closed) return false;
    // Enqueue even when the queue is already at the mark: a committed entry is
    // never dropped. The close below then lets the reader drain it and resume
    // from its own `id:` cursor.
    sink.enqueue(encoder.encode(frame));
    if (frame.startsWith("id: ")) cursor = true;
    // Let the snapshot complete before treating its non-cursor preface as
    // backpressure. A thread snapshot may still advance on its first entry.
    if (
      (sink.desiredSize ?? 0) <= 0 &&
      (cursor || snapshotComplete)
    )
      closeForBackpressure();
    return true;
  };

  const preview: PreviewEmit = (text) => {
    if (closed || sink === null) return false;
    pending = { text };
    if ((sink.desiredSize ?? 0) > 0) flushPreview();
    return !closed;
  };

  const body = new ReadableStream<Uint8Array>(
    {
      start(controller) {
        sink = controller;
        const fail = (error: unknown) => {
          if (!closed) {
            closed = true;
            pending = null;
            resumeInitialDrain();
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
            pending = null;
            resumeInitialDrain();
            controller.close();
          }
        }, fail);
        try {
          format.snapshot(emit, preview);
          snapshotComplete = true;
          // A snapshot large enough to cross the queue mark already closed and
          // stopped the watch; starting it now would throw.
          if (closed) return;
          // The snapshot's preview is the connection's starting state: enqueue
          // it even if the body is already backpressured, so a reconnection
          // always learns the current transient text (or its clear). Later
          // previews coalesce; this one frame is bounded per connection.
          flushPreview();
          if (options?.overflow !== "after-cursor" && (controller.desiredSize ?? 0) <= 0) {
            const drain = Promise.withResolvers<void>();
            initialDrain = drain.promise;
            releaseInitialDrain = drain.resolve;
          }
          watch.start(async (events) => {
            if (initialDrain !== null) await initialDrain;
            if (!closed) await format.events(events, emit, preview);
          });
        } catch (error) {
          fail(error);
          void watch.stop();
        }
      },
      pull(controller) {
        // The reader is ready for more, so the newest coalesced preview goes
        // now; a slow reader simply gets the latest text next time.
        if ((controller.desiredSize ?? 0) > 0) resumeInitialDrain();
        flushPreview();
      },
      async cancel() {
        closed = true;
        pending = null;
        resumeInitialDrain();
        await watch.stop();
      },
    },
    { highWaterMark: MAX_QUEUED_BYTES, size: (chunk) => chunk.byteLength },
  );
  return new Response(body, {
    headers: {
      "content-type": "text/event-stream",
      "cache-control": "no-store",
    },
  });
}
