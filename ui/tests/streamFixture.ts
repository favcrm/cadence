import type { MasterState } from "../src/lib/types";

const IDLE: MasterState = { alias: "master", provider: "fixture", endpoint_kind: "fixture", live: false, turn: null, queue: [] };

/** Only the master transcript carries a turn snapshot. The board and member
 * streams retain their own protocols; an empty fixture keeps them open. */
export function streamFixture(path: string, master: MasterState = IDLE, signal?: AbortSignal | null): Response {
  const isMaster = new URL(path, "http://localhost").pathname === "/api/threads/master/stream";
  let closed = false;
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      if (isMaster) controller.enqueue(new TextEncoder().encode(`event: master\ndata: ${JSON.stringify(master)}\n\n`));
      const close = () => { if (!closed) { closed = true; controller.close(); } };
      if (signal?.aborted) close();
      else signal?.addEventListener("abort", close, { once: true });
    },
    cancel() { closed = true; },
  });
  return new Response(body, { headers: { "Content-Type": "text/event-stream" } });
}
