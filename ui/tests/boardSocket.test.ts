import { api, ApiError } from "../src/lib/api";
import { parseBoardSocketTarget, resetBoardSocketForTests } from "../src/lib/boardSocket";
import { allowPrivateReads, pausePrivateReads } from "../src/lib/privateSession";
import { setSessionKey } from "../src/lib/sessionKey";
import { subscribeSse, type SseErrorState } from "../src/lib/sse";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  readyState = 0;
  readonly sent: string[] = [];
  closedWith: { code?: number; reason?: string } | null = null;
  private readonly listeners = new Map<string, Set<(event: unknown) => void>>();

  constructor(readonly url: string) {
    FakeWebSocket.instances.push(this);
  }

  addEventListener(type: string, listener: (event: unknown) => void): void {
    let set = this.listeners.get(type);
    if (!set) {
      set = new Set();
      this.listeners.set(type, set);
    }
    set.add(listener);
  }

  send(data: string): void {
    if (this.readyState !== 1) throw new Error("WebSocket is not open");
    this.sent.push(data);
  }

  close(code?: number, reason?: string): void {
    if (this.readyState === 3) return;
    this.readyState = 3;
    this.closedWith = { code, reason };
    this.emit("close", { code: code ?? 1000, reason: reason ?? "" });
  }

  serverOpenAndWelcome(build = "build-ws-1"): void {
    this.readyState = 1;
    this.emit("open", {});
    this.serverSend({ v: 1, type: "welcome", build });
  }

  serverSend(frame: Record<string, unknown> | string): void {
    const data = typeof frame === "string" ? frame : JSON.stringify(frame);
    this.emit("message", { data });
  }

  serverClose(code = 1006, reason = ""): void {
    this.readyState = 3;
    this.emit("close", { code, reason });
  }

  private emit(type: string, event: unknown): void {
    for (const listener of [...(this.listeners.get(type) ?? [])]) {
      listener(event);
    }
  }
}

function installBrowserEnvironment(host = "demo-company.cadencecloud.app"): {
  events: string[];
  restore(): void;
} {
  const oldWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const oldWebSocket = Object.getOwnPropertyDescriptor(globalThis, "WebSocket");
  const oldLocalStorage = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  const oldSessionStorage = Object.getOwnPropertyDescriptor(globalThis, "sessionStorage");

  const local = new Map<string, string>();
  const session = new Map<string, string>();
  const dispatched: string[] = [];
  const target = new EventTarget();
  target.addEventListener("cadence:session-required", () => {
    dispatched.push("cadence:session-required");
  });

  const makeStore = (map: Map<string, string>) => ({
    getItem: (key: string) => map.get(key) ?? null,
    setItem: (key: string, value: string) => {
      map.set(key, value);
    },
    removeItem: (key: string) => {
      map.delete(key);
    },
  });

  const fakeWindow = {
    location: { protocol: "https:", host },
    dispatchEvent: (event: Event) => target.dispatchEvent(event),
    addEventListener: target.addEventListener.bind(target),
    removeEventListener: target.removeEventListener.bind(target),
  };

  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: makeStore(local) });
  Object.defineProperty(globalThis, "sessionStorage", { configurable: true, value: makeStore(session) });
  Object.defineProperty(globalThis, "window", { configurable: true, value: fakeWindow });
  Object.defineProperty(globalThis, "WebSocket", { configurable: true, value: FakeWebSocket });
  FakeWebSocket.instances = [];

  return {
    events: dispatched,
    restore() {
      resetBoardSocketForTests();
      if (oldWindow) Object.defineProperty(globalThis, "window", oldWindow);
      else Reflect.deleteProperty(globalThis, "window");
      if (oldWebSocket) Object.defineProperty(globalThis, "WebSocket", oldWebSocket);
      else Reflect.deleteProperty(globalThis, "WebSocket");
      if (oldLocalStorage) Object.defineProperty(globalThis, "localStorage", oldLocalStorage);
      else Reflect.deleteProperty(globalThis, "localStorage");
      if (oldSessionStorage) Object.defineProperty(globalThis, "sessionStorage", oldSessionStorage);
      else Reflect.deleteProperty(globalThis, "sessionStorage");
    },
  };
}

const flushMicrotasks = async () => {
  await Promise.resolve();
  await Promise.resolve();
};

async function main() {
  allowPrivateReads();

  // 1. Target parsing matches only hosted board feed and master thread streams.
  equal(
    parseBoardSocketTarget("/api/stream?entities=1&accept_degraded=1"),
    { kind: "board", entities: true, acceptDegraded: true },
    "board stream target",
  );
  equal(
    parseBoardSocketTarget("/api/threads/master/stream?conversation=tc_abc&after=12"),
    { kind: "thread", alias: "master", conversation: "tc_abc", after: 12 },
    "master thread stream target",
  );
  equal(parseBoardSocketTarget("/api/threads/pm/stream?after=4"), null, "non-master thread stays on HTTP");

  // 2. Hosted cookie tab multiplexes board stream + master thread stream + threadSend onto ONE socket.
  {
    const env = installBrowserEnvironment();
    const oldFetch = globalThis.fetch;
    let fetchCalls = 0;
    globalThis.fetch = async () => {
      fetchCalls += 1;
      throw new Error("Unexpected HTTP fetch while WebSocket is healthy");
    };
    try {
      const boardEvents: string[] = [];
      const threadEvents: string[] = [];
      const builds: string[] = [];
      let boardOpened = false;
      let threadOpened = false;

      const boardSub = subscribeSse({
        url: "/api/stream?entities=1&accept_degraded=1",
        events: ["issues", "agents"],
        onOpen: () => {
          boardOpened = true;
        },
        onBuild: (b) => builds.push(b),
        onEvent: (e) => boardEvents.push(`${e.type}:${e.data}`),
      });

      const threadSub = subscribeSse({
        url: "/api/threads/master/stream?conversation=tc_123",
        lastEventId: "4",
        events: ["master", "preview", "entry"],
        onOpen: () => {
          threadOpened = true;
        },
        onEvent: (e) => threadEvents.push(`${e.type}:${e.id}:${e.data}`),
      });

      equal(FakeWebSocket.instances.length, 1, "one shared WebSocket per tab");
      const ws = FakeWebSocket.instances[0];
      equal(ws.url, "wss://demo-company.cadencecloud.app/api/ws", "hosted wss endpoint");

      ws.serverOpenAndWelcome("build-2026");
      equal(ws.sent.length, 2, "both subscriptions sent upon welcome");
      const subBoardCmd = JSON.parse(ws.sent[0]) as { subId: string; type: string; entities: boolean; acceptDegraded: boolean };
      const subThreadCmd = JSON.parse(ws.sent[1]) as { subId: string; type: string; alias: string; conversation: string; after: number };
      equal(subBoardCmd.type, "subscribe_board", "first command is subscribe_board");
      equal(subBoardCmd.entities && subBoardCmd.acceptDegraded, true, "board flags forwarded");
      equal(subThreadCmd.type, "subscribe_thread", "second command is subscribe_thread");
      equal(subThreadCmd.conversation, "tc_123", "conversation selector forwarded");
      equal(subThreadCmd.after, 4, "resume cursor forwarded");

      ws.serverSend({ v: 1, type: "stream_open", subId: subBoardCmd.subId });
      ws.serverSend({
        v: 1,
        type: "stream_event",
        subId: subBoardCmd.subId,
        event: "hello",
        data: JSON.stringify({ build: "build-2026" }),
      });
      ws.serverSend({
        v: 1,
        type: "stream_event",
        subId: subBoardCmd.subId,
        event: "issues",
        data: "{\"resources\":[\"issues\"]}",
      });

      ws.serverSend({ v: 1, type: "stream_open", subId: subThreadCmd.subId });
      ws.serverSend({
        v: 1,
        type: "stream_event",
        subId: subThreadCmd.subId,
        event: "preview",
        data: "\"Hello \"",
      });
      ws.serverSend({
        v: 1,
        type: "stream_event",
        subId: subThreadCmd.subId,
        event: "entry",
        id: "5",
        data: "{\"seq\":5}",
      });
      // Subsequent id-less frame inherits sticky `lastEventId` just like native EventSource.
      ws.serverSend({
        v: 1,
        type: "stream_event",
        subId: subThreadCmd.subId,
        event: "preview",
        data: "null",
      });

      equal(boardOpened && threadOpened, true, "both streams report open");
      equal(builds, ["build-2026"], "hello frame updates build");
      equal(boardEvents, ["issues:{\"resources\":[\"issues\"]}"], "board invalidation delivered");
      equal(
        threadEvents,
        ["preview:null:\"Hello \"", "entry:5:{\"seq\":5}", "preview:5:null"],
        "thread frames preserve order and sticky lastEventId",
      );
      equal(threadSub.lastEventId(), "5", "thread resume cursor advances to 5");

      // Send a master message over the open socket (0 HTTP POST calls).
      const sendPromise = api.threadSend("master", "Ship it", "msg_1", undefined, undefined, "tc_123");
      await flushMicrotasks();
      equal(ws.sent.length, 3, "send_message transmitted on existing WebSocket");
      const sendCmd = JSON.parse(ws.sent[2]) as { id: string; type: string; alias: string; payload: Record<string, unknown> };
      equal(sendCmd.type, "send_message", "send_message command type");
      equal(sendCmd.payload, { text: "Ship it", message: "msg_1", conversation: "tc_123" }, "send_message payload");
      ws.serverSend({
        v: 1,
        id: sendCmd.id,
        type: "command_result",
        status: 200,
        body: { ok: true, message: "msg_1" },
      });
      const sendReceipt = await sendPromise;
      equal(sendReceipt, { ok: true, message: "msg_1" }, "send_message resolves receipt");

      // Server 409 error over socket surfaces as ApiError with status & code.
      const errPromise = api.threadSend("master", "Conflict", "msg_1", undefined, undefined, "tc_123");
      await flushMicrotasks();
      const errCmd = JSON.parse(ws.sent[3]) as { id: string };
      ws.serverSend({
        v: 1,
        id: errCmd.id,
        type: "command_result",
        status: 409,
        body: { error: { code: "message_conflict", message: "Message id conflict." } },
      });
      let caught: unknown = null;
      try {
        await errPromise;
      } catch (err) {
        caught = err;
      }
      equal(caught instanceof ApiError && caught.status === 409 && caught.code === "message_conflict", true, "command_result 409 throws ApiError");

      // Closing a subscription sends `unsubscribe` without tearing down the sibling subscription.
      threadSub.close();
      equal(ws.sent.length, 5, "unsubscribe sent for closed thread subscription");
      equal(JSON.parse(ws.sent[4]).type, "unsubscribe", "unsubscribe frame");
      equal(fetchCalls, 0, "zero HTTP fetches used");

      // Server revocation code 4001 dispatches cadence:session-required.
      ws.serverClose(4001, "session_revoked");
      equal(env.events, ["cadence:session-required"], "4001 close notifies session-required");

      boardSub.close();
    } finally {
      globalThis.fetch = oldFetch;
      env.restore();
    }
  }

  // 3. Handshake failure before `welcome` (e.g. older Worker build) seamlessly delegates to HTTP SSE,
  //    and local daemon (`sessionKey() !== null`) bypasses WebSocket completely.
  {
    const env = installBrowserEnvironment();
    const oldFetch = globalThis.fetch;
    const fetchedUrls: string[] = [];
    let sseController!: ReadableStreamDefaultController<Uint8Array>;
    globalThis.fetch = async (input) => {
      fetchedUrls.push(String(input));
      const body = new ReadableStream<Uint8Array>({
        start(c) {
          sseController = c;
        },
      });
      return new Response(body, { headers: { "Content-Type": "text/event-stream" } });
    };
    try {
      const seen: string[] = [];
      const errors: SseErrorState[] = [];
      let delivered!: () => void;
      const gotFrame = new Promise<void>((resolve) => {
        delivered = resolve;
      });

      const sub = subscribeSse({
        url: "/api/stream?entities=1&accept_degraded=1",
        events: ["issues"],
        onEvent: (e) => {
          seen.push(e.data);
          delivered();
        },
        onError: (s) => errors.push(s),
      });

      equal(FakeWebSocket.instances.length, 1, "attempted WebSocket upgrade first");
      // Older worker rejects `/api/ws` before sending `welcome`.
      FakeWebSocket.instances[0].serverClose(1006, "upgrade_rejected");
      await flushMicrotasks();

      equal(fetchedUrls, ["/api/stream?entities=1&accept_degraded=1"], "delegated immediately to HTTP SSE");
      equal(errors, [], "pre-welcome fallback causes zero visible SSE error flash");
      sseController.enqueue(new TextEncoder().encode("event: issues\ndata: {\"ok\":true}\n\n"));
      await gotFrame;
      equal(seen, ["{\"ok\":true}"], "HTTP fallback stream delivers frames");
      sub.close();

      // Local native daemon sets `sessionKey`: WebSocket is skipped so `X-Cadence-Session` stays on HTTP.
      resetBoardSocketForTests();
      FakeWebSocket.instances = [];
      setSessionKey("local-daemon-session-key");
      const localSub = subscribeSse({
        url: "/api/stream?entities=1&accept_degraded=1",
        events: ["issues"],
        onEvent: () => {},
      });
      equal(FakeWebSocket.instances.length, 0, "local sessionKey skips WebSocket");
      localSub.close();
      setSessionKey(null);

      // Pause hook tears down an open socket immediately.
      pausePrivateReads();
      allowPrivateReads();
    } finally {
      setSessionKey(null);
      allowPrivateReads();
      globalThis.fetch = oldFetch;
      env.restore();
    }
  }
  console.log("boardSocket checks passed");
}

main().catch((e) => {
  setTimeout(() => {
    throw e;
  });
});
