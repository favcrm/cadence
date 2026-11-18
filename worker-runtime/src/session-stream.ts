import type { WatchCall, WatchHandle } from "@earendil-works/chord/watch";
import type { ConversationView } from "@earendil-works/pi-durable";

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
 * The owner-only diagnostic stream carries engine views, not the board
 * projection. Keep image markers but never serialize their bytes.
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

/** One connection's projection of engine views. */
export interface SessionStreamFormat {
  snapshot(emit: Emit, preview: PreviewEmit): void;
  change(
    change: Extract<WatchCall<ConversationView>, { kind: "value" }>,
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
   * The diagnostic stream instead pauses its watch when a full view fills the
   * queue. The watch coalesces updates until the reader drains that frame.
   */
  readonly overflow?: "after-cursor";
}

/**
 * Both wire formats own the same watch lifecycle and cancellation rules.
 *
 * The returned body closes when the engine retires the watch, fails on a
 * watch error, and stops the subscription when the client cancels. A slow
 * thread reader drains its queued bytes and reconnects from its durable
 * cursor. Its oversized initial header still lets the first entry through.
 * A diagnostic view has no cursor, so its watch pauses at the queue limit
 * instead; the engine coalesces updates until the reader drains the view.
 *
 * A preview frame is transient: only the newest one matters, so a slow reader
 * never accumulates a frame per streamed chunk. At most one preview waits
 * outside the stream at any time; a later preview replaces it, and the stream
 * flushes it before the next durable frame (or on `pull`). That keeps a
 * coalesced preview ahead of the entry it belongs to without a timer.
 */
export function sessionEventResponse(
  watch: Pick<WatchHandle<ConversationView>, "start" | "stop" | "closed">,
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
  let drain: Promise<void> | null = null;
  let releaseDrain: (() => void) | null = null;
  const pauseForDrain = (): void => {
    if (drain !== null) return;
    const wait = Promise.withResolvers<void>();
    drain = wait.promise;
    releaseDrain = wait.resolve;
  };
  const resumeDrain = (): void => {
    const release = releaseDrain;
    releaseDrain = null;
    drain = null;
    release?.();
  };

  /** End this response after its queued bytes drain; the client reconnects
   *  with the last durable cursor it received. */
  const closeForBackpressure = (): void => {
    if (closed || sink === null) return;
    closed = true;
    pending = null;
    resumeDrain();
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
    if ((sink.desiredSize ?? 0) <= 0 && (cursor || snapshotComplete)) {
      if (options?.overflow === "after-cursor") closeForBackpressure();
      else pauseForDrain();
    }
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
            resumeDrain();
            controller.error(error);
          }
        };
        // Observe termination before attaching the consumer, including a watch
        // that was already closed when its snapshot was obtained.
        void watch.closed.then((end) => {
          if (
            end.kind === "failed" ||
            end.kind === "listener_error"
          ) {
            fail(end.error);
            return;
          }
          if (!closed) {
            closed = true;
            pending = null;
            resumeDrain();
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
          if (options?.overflow !== "after-cursor" && (controller.desiredSize ?? 0) <= 0)
            pauseForDrain();
          watch.start(async (change) => {
            if (drain !== null) await drain;
            if (closed) return;
            if (change.kind === "failed") throw change.error;
            if (change.retired) throw new Error("Conversation view retired");
            await format.change(change, emit, preview);
            if (drain !== null) await drain;
          });
        } catch (error) {
          fail(error);
          void watch.stop();
        }
      },
      pull(controller) {
        // The reader is ready for more, so the newest coalesced preview goes
        // now; a slow reader simply gets the latest text next time.
        if ((controller.desiredSize ?? 0) > 0) resumeDrain();
        flushPreview();
      },
      async cancel() {
        closed = true;
        pending = null;
        resumeDrain();
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
