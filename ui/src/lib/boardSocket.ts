import {
  beginPrivateOperation,
  bindPrivateAbort,
  notifyPrivateSessionRequired,
  onPrivateReadsPaused,
  privateReadsAllowed,
  privateSessionGeneration,
  usingPrivateOperation,
  type PrivateOperation,
} from "./privateSession";
import { sessionKey } from "./sessionKey";
import type { EventSourceLike } from "./sse";

const WS_CONNECTING = 0;
const WS_OPEN = 1;
const WS_CLOSED = 3;
const ES_OPEN = 1;
const ES_CLOSED = 2;

const PING_INTERVAL_MS = 25_000;
const COMMAND_TIMEOUT_MS = 15_000;
const UNSUPPORTED_COOLDOWN_MS = 15_000;
const IDLE_CLOSE_GRACE_MS = 50;
const MAX_CLIENT_FRAME_BYTES = 38_000;

type SocketTarget =
  | { kind: "board"; entities: boolean; acceptDegraded: boolean }
  | { kind: "thread"; alias: "master"; conversation?: string; after?: number };

type ServerFrame =
  | { v: 1; type: "welcome"; build: string }
  | { v: 1; id?: string; type: "stream_open"; subId: string }
  | { v: 1; type: "stream_event"; subId: string; event: string; data: string; id?: string }
  | { v: 1; id?: string; type: "stream_error"; subId: string; status: number; code: string; message?: string }
  | { v: 1; type: "stream_closed"; subId: string; reason: string }
  | { v: 1; id: string; type: "command_result"; status: number; body: unknown }
  | { v: 1; type: "revoked"; reason: string }
  | { v: 1; id?: string; type: "error"; code: string; message?: string };

interface ActiveSubscription {
  readonly subId: string;
  readonly target: SocketTarget;
  subscribeSent: boolean;
  onOpen(): void;
  onEvent(event: string, data: string, id?: string): void;
  onStreamError(status: number, code: string): void;
  onStreamClosed(): void;
  onFallbackToHttp(): void;
}

interface PendingCommand {
  resolve(result: { status: number; body: unknown } | null): void;
  cleanup(): void;
}

type TimerHandle = ReturnType<typeof setTimeout>;

let unsupportedUntil = 0;
let nextSeq = 0;

function nextId(prefix: string): string {
  nextSeq += 1;
  return `${prefix}_${nextSeq}`;
}

function unrefTimer(timer: unknown): void {
  if (typeof timer === "object" && timer !== null && "unref" in timer) {
    const fn = Reflect.get(timer, "unref");
    if (typeof fn === "function") fn.call(timer);
  }
}

/**
 * Hosted boards authenticate with an HttpOnly same-origin cookie (`sessionKey() === null`),
 * whereas the local native daemon requires `X-Cadence-Session` on HTTP requests (which
 * browser `new WebSocket` cannot attach) and credential-entry documents run under
 * `__CADENCE_ENTRY_MODE__`. Only hosted cookie-authenticated browser tabs open `/api/ws`.
 */
function canUseBoardSocket(): boolean {
  if (typeof window === "undefined" || typeof WebSocket !== "function") return false;
  if ((globalThis as { __CADENCE_ENTRY_MODE__?: boolean }).__CADENCE_ENTRY_MODE__ === true) return false;
  if (sessionKey() !== null || !privateReadsAllowed()) return false;
  const proto = window.location?.protocol;
  const host = window.location?.host;
  if ((proto !== "https:" && proto !== "http:") || typeof host !== "string" || host.length === 0) return false;
  return Date.now() >= unsupportedUntil;
}

function parseBoardSocketTarget(url: string): SocketTarget | null {
  if (!url.startsWith("/") || url.startsWith("//")) return null;
  const qIdx = url.indexOf("?");
  const path = qIdx < 0 ? url : url.slice(0, qIdx);
  const query = qIdx < 0 ? "" : url.slice(qIdx + 1);
  const params = new URLSearchParams(query);

  if (path === "/api/stream") {
    for (const key of params.keys()) {
      if (key !== "entities" && key !== "accept_degraded") return null;
    }
    return {
      kind: "board",
      entities: params.get("entities") === "1",
      acceptDegraded: params.get("accept_degraded") === "1",
    };
  }

  if (path === "/api/threads/master/stream") {
    for (const key of params.keys()) {
      if (key !== "conversation" && key !== "after") return null;
    }
    if (params.getAll("conversation").length > 1 || params.getAll("after").length > 1) {
      return null;
    }
    let after: number | undefined;
    if (params.has("after")) {
      const rawAfter = params.get("after") ?? "";
      if (!/^\d+$/.test(rawAfter)) return null;
      const parsed = Number(rawAfter);
      if (!Number.isSafeInteger(parsed) || parsed < 0) return null;
      after = parsed;
    }
    let conversation: string | undefined;
    if (params.has("conversation")) {
      const rawConversation = params.get("conversation") ?? "";
      // Explicit empty `?conversation=` must never silently open Home; delegate
      // to HTTP so the server returns its canonical `400 validation_failed`.
      if (rawConversation.length === 0) return null;
      conversation = rawConversation;
    }
    return {
      kind: "thread",
      alias: "master",
      ...(conversation !== undefined ? { conversation } : {}),
      ...(after !== undefined ? { after } : {}),
    };
  }

  return null;
}

function parseServerFrame(raw: unknown): ServerFrame | null {
  if (typeof raw !== "string" || raw === "pong") return null;
  try {
    const decoded: unknown = JSON.parse(raw);
    if (decoded === null || typeof decoded !== "object" || Array.isArray(decoded)) return null;
    const record = decoded as Record<string, unknown>;
    return record.v === 1 && typeof record.type === "string" ? (record as unknown as ServerFrame) : null;
  } catch {
    return null;
  }
}

class SharedBoardSocket {
  readonly ws: WebSocket;
  readonly generation: number;
  welcomed = false;
  closed = false;
  private pingTimer: TimerHandle | null = null;
  private idleTimer: TimerHandle | null = null;
  private readonly subscriptions = new Map<string, ActiveSubscription>();
  private readonly commands = new Map<string, PendingCommand>();

  constructor() {
    this.generation = privateSessionGeneration();
    const scheme = window.location.protocol === "https:" ? "wss:" : "ws:";
    this.ws = new WebSocket(`${scheme}//${window.location.host}/api/ws`);
    this.ws.addEventListener("message", (event) => this.handleMessage(event.data));
    this.ws.addEventListener("error", () => {
      if (!this.welcomed && this.ws.readyState === WS_CLOSED) this.teardown("socket_error");
    });
    this.ws.addEventListener("close", (event) => {
      if (event.code === 4001 && event.reason === "session_revoked") {
        this.dispatchSessionRequired();
      }
      this.teardown("socket_closed");
    });
  }

  isReady(): boolean {
    return (
      !this.closed &&
      this.welcomed &&
      this.ws.readyState === WS_OPEN &&
      this.generation === privateSessionGeneration()
    );
  }

  addSubscription(sub: ActiveSubscription): void {
    this.clearIdleTimer();
    this.subscriptions.set(sub.subId, sub);
    if (this.isReady()) {
      queueMicrotask(() => {
        if (this.subscriptions.get(sub.subId) === sub) this.sendSubscribe(sub);
      });
    }
  }

  removeSubscription(subId: string, notifyServer: boolean): void {
    const existing = this.subscriptions.get(subId);
    if (!existing) return;
    this.subscriptions.delete(subId);
    if (notifyServer && existing.subscribeSent && this.isReady()) {
      this.sendJson({ v: 1, id: nextId("unsub"), type: "unsubscribe", subId });
    }
    this.scheduleIdleCheck();
  }

  sendCommand(
    frame: Record<string, unknown> & { id: string },
    signal: AbortSignal,
  ): Promise<{ status: number; body: unknown } | null> {
    if (!this.isReady()) return Promise.resolve(null);
    const encoded = JSON.stringify(frame);
    if (new TextEncoder().encode(encoded).byteLength > MAX_CLIENT_FRAME_BYTES) return Promise.resolve(null);
    this.clearIdleTimer();
    return new Promise((resolve) => {
      let settled = false;
      const finish = (value: { status: number; body: unknown } | null) => {
        if (settled) return;
        settled = true;
        cleanup();
        this.commands.delete(frame.id);
        this.scheduleIdleCheck();
        resolve(value);
      };
      const onAbort = () => finish(null);
      const timer = setTimeout(() => finish(null), COMMAND_TIMEOUT_MS);
      unrefTimer(timer);
      const cleanup = () => {
        clearTimeout(timer);
        signal.removeEventListener("abort", onAbort);
      };
      signal.addEventListener("abort", onAbort, { once: true });
      this.commands.set(frame.id, { resolve: finish, cleanup });
      try {
        this.ws.send(encoded);
      } catch {
        finish(null);
      }
    });
  }

  private dispatchSessionRequired(): void {
    if (!this.closed && privateReadsAllowed() && this.generation === privateSessionGeneration() && typeof window !== "undefined") {
      window.dispatchEvent(new Event("cadence:session-required"));
    }
  }

  private sendSubscribe(sub: ActiveSubscription): void {
    if (sub.subscribeSent || !this.isReady()) return;
    sub.subscribeSent = true;
    const sent =
      sub.target.kind === "board"
        ? this.sendJson({
            v: 1,
            id: sub.subId,
            subId: sub.subId,
            type: "subscribe_board",
            entities: sub.target.entities,
            acceptDegraded: sub.target.acceptDegraded,
          })
        : this.sendJson({
            v: 1,
            id: sub.subId,
            subId: sub.subId,
            type: "subscribe_thread",
            alias: sub.target.alias,
            ...(sub.target.conversation !== undefined ? { conversation: sub.target.conversation } : {}),
            ...(sub.target.after !== undefined ? { after: sub.target.after } : {}),
          });
    if (!sent) {
      this.subscriptions.delete(sub.subId);
      queueMicrotask(() => sub.onStreamClosed());
    }
  }

  private sendJson(payload: Record<string, unknown>): boolean {
    try {
      this.ws.send(JSON.stringify(payload));
      return true;
    } catch {
      return false;
    }
  }

  private startPing(): void {
    this.stopPing();
    const timer = setInterval(() => {
      if (!this.isReady()) return;
      try {
        this.ws.send("ping");
      } catch {
        // Closing socket will fire `close`.
      }
    }, PING_INTERVAL_MS);
    unrefTimer(timer);
    this.pingTimer = timer;
  }

  private stopPing(): void {
    if (this.pingTimer !== null) {
      clearInterval(this.pingTimer);
      this.pingTimer = null;
    }
  }

  private clearIdleTimer(): void {
    if (this.idleTimer !== null) {
      clearTimeout(this.idleTimer);
      this.idleTimer = null;
    }
  }

  private scheduleIdleCheck(): void {
    this.clearIdleTimer();
    if (this.closed || this.subscriptions.size > 0 || this.commands.size > 0) return;
    const timer = setTimeout(() => {
      this.idleTimer = null;
      if (this.subscriptions.size === 0 && this.commands.size === 0) this.teardown("idle");
    }, IDLE_CLOSE_GRACE_MS);
    unrefTimer(timer);
    this.idleTimer = timer;
  }

  private handleMessage(raw: unknown): void {
    if (this.closed || this.generation !== privateSessionGeneration()) return;
    const frame = parseServerFrame(raw);
    if (!frame) return;

    switch (frame.type) {
      case "welcome":
        if (this.welcomed) return;
        this.welcomed = true;
        unsupportedUntil = 0;
        this.startPing();
        for (const sub of [...this.subscriptions.values()]) this.sendSubscribe(sub);
        return;
      case "stream_open":
        this.subscriptions.get(frame.subId)?.onOpen();
        return;
      case "stream_event":
        this.subscriptions.get(frame.subId)?.onEvent(frame.event, frame.data, frame.id);
        return;
      case "stream_error": {
        const sub = this.subscriptions.get(frame.subId);
        if (!sub) return;
        this.subscriptions.delete(frame.subId);
        sub.onStreamError(frame.status, frame.code);
        this.scheduleIdleCheck();
        return;
      }
      case "stream_closed": {
        const sub = this.subscriptions.get(frame.subId);
        if (!sub) return;
        this.subscriptions.delete(frame.subId);
        sub.onStreamClosed();
        this.scheduleIdleCheck();
        return;
      }
      case "command_result":
        this.commands.get(frame.id)?.resolve({ status: frame.status, body: frame.body });
        return;
      case "revoked":
        // Only local session revocation (`session_revoked`) ends the browser session;
        // `membership_revoked` also fires on 409 placement changes and 503 blips,
        // which must reconnect / recheck rather than wiping unsent drafts.
        if (frame.reason === "session_revoked") this.dispatchSessionRequired();
        this.teardown("revoked");
        return;
      case "error":
        if (frame.id) {
          this.commands.get(frame.id)?.resolve({
            status: 400,
            body: { error: { code: frame.code, message: frame.message ?? frame.code } },
          });
          const sub = this.subscriptions.get(frame.id);
          if (sub) {
            this.subscriptions.delete(frame.id);
            sub.onStreamError(400, frame.code);
            this.scheduleIdleCheck();
          }
        }
        return;
    }
  }

  teardown(reason: "socket_closed" | "socket_error" | "session_paused" | "revoked" | "idle" | "reset"): void {
    if (this.closed) return;
    this.closed = true;
    if (activeSocket === this) activeSocket = null;
    this.stopPing();
    this.clearIdleTimer();

    const failedBeforeWelcome = !this.welcomed && (reason === "socket_closed" || reason === "socket_error");
    if (failedBeforeWelcome) unsupportedUntil = Date.now() + UNSUPPORTED_COOLDOWN_MS;

    const subs = [...this.subscriptions.values()];
    this.subscriptions.clear();
    const cmds = [...this.commands.values()];
    this.commands.clear();

    for (const cmd of cmds) {
      cmd.cleanup();
      cmd.resolve(null);
    }
    for (const sub of subs) {
      if (failedBeforeWelcome) sub.onFallbackToHttp();
      else sub.onStreamClosed();
    }
    try {
      if (this.ws.readyState === WS_CONNECTING || this.ws.readyState === WS_OPEN) {
        this.ws.close(1000, reason);
      }
    } catch {
      // Ignore already-closing socket errors.
    }
  }
}

let activeSocket: SharedBoardSocket | null = null;
let pauseRegistered = false;

function getOrCreateSocket(): SharedBoardSocket {
  if (!pauseRegistered) {
    pauseRegistered = true;
    onPrivateReadsPaused(() => activeSocket?.teardown("session_paused"));
  }
  if (
    activeSocket !== null &&
    !activeSocket.closed &&
    activeSocket.generation === privateSessionGeneration() &&
    (activeSocket.ws.readyState === WS_CONNECTING || activeSocket.ws.readyState === WS_OPEN)
  ) {
    return activeSocket;
  }
  activeSocket?.teardown("reset");
  activeSocket = new SharedBoardSocket();
  return activeSocket;
}

export function openBoardSocketStream(
  url: string,
  fallback: (url: string) => EventSourceLike,
): EventSourceLike {
  const target = parseBoardSocketTarget(url);
  if (target === null || !canUseBoardSocket()) return fallback(url);

  const generation = privateSessionGeneration();
  const abortController = new AbortController();
  let operation: PrivateOperation | null = null;
  let stopFollow = () => {};
  try {
    operation = beginPrivateOperation();
    stopFollow = bindPrivateAbort(abortController, operation.signal);
  } catch {
    return fallback(url);
  }
  if (abortController.signal.aborted) {
    stopFollow();
    operation.dispose();
    return fallback(url);
  }

  const listeners = new Map<string, Set<(event: MessageEvent<string>) => void>>();
  let delegate: EventSourceLike | null = null;
  let terminalStatus: number | null = null;
  let closed = false;
  let lastId = "";
  const subId = nextId("s");
  const socket = getOrCreateSocket();

  const cleanupOperation = () => {
    stopFollow();
    operation?.dispose();
    operation = null;
  };

  const failClosed = () => {
    if (closed) return;
    closed = true;
    cleanupOperation();
    Object.assign(source, { readyState: ES_CLOSED });
    source.onerror?.(new Event("error"));
  };

  const shutdown = (notifyServer: boolean) => {
    if (closed) return;
    closed = true;
    cleanupOperation();
    if (delegate) {
      delegate.close();
      delegate = null;
    } else {
      socket.removeSubscription(subId, notifyServer);
    }
    Object.assign(source, { readyState: ES_CLOSED });
  };

  const source: EventSourceLike = {
    readyState: 0,
    onopen: null,
    onerror: null,
    lastStatus: () => (delegate?.lastStatus ? delegate.lastStatus() : terminalStatus),
    addEventListener(type, listener) {
      let set = listeners.get(type);
      if (!set) {
        set = new Set();
        listeners.set(type, set);
      }
      set.add(listener);
      delegate?.addEventListener(type, listener);
    },
    close: () => shutdown(true),
  };

  abortController.signal.addEventListener("abort", () => shutdown(false), { once: true });

  socket.addSubscription({
    subId,
    target,
    subscribeSent: false,
    onOpen() {
      if (closed || !operation || generation !== privateSessionGeneration()) return;
      try {
        operation.assertCurrent();
      } catch {
        return;
      }
      Object.assign(source, { readyState: ES_OPEN });
      source.onopen?.(new Event("open"));
    },
    onEvent(eventType, data, id) {
      if (closed || !operation || generation !== privateSessionGeneration()) return;
      if (id !== undefined && !id.includes("\0")) lastId = id;
      const event = { data, lastEventId: lastId } as MessageEvent<string>;
      for (const listener of listeners.get(eventType) ?? []) {
        try {
          operation.assertCurrent();
        } catch {
          return;
        }
        listener(event);
      }
    },
    onStreamError(status, code) {
      if (closed) return;
      terminalStatus = status;
      if (operation && status === 401) notifyPrivateSessionRequired(status, { code }, operation);
      failClosed();
    },
    onStreamClosed: failClosed,
    onFallbackToHttp() {
      if (closed || !privateReadsAllowed() || generation !== privateSessionGeneration()) {
        failClosed();
        return;
      }
      cleanupOperation();
      const httpSource = fallback(url);
      delegate = httpSource;
      Object.assign(source, { readyState: httpSource.readyState });
      for (const [type, set] of listeners.entries()) {
        for (const listener of set) httpSource.addEventListener(type, listener);
      }
      httpSource.onopen = (event) => {
        if (closed || delegate !== httpSource) return;
        Object.assign(source, { readyState: httpSource.readyState });
        source.onopen?.(event);
      };
      httpSource.onerror = (event) => {
        if (closed || delegate !== httpSource) return;
        Object.assign(source, { readyState: httpSource.readyState });
        source.onerror?.(event);
      };
    },
  });

  return source;
}

export async function sendMasterMessageViaSocket(
  alias: string,
  payload: Record<string, unknown>,
  operation?: PrivateOperation,
): Promise<{ status: number; body: unknown } | null> {
  if (alias !== "master" || !canUseBoardSocket()) return null;
  const socket = activeSocket;
  if (socket === null || !socket.isReady()) return null;

  return usingPrivateOperation(operation, async (op) => {
    if (!socket.isReady()) return null;
    const result = await socket.sendCommand(
      { v: 1, id: nextId("msg"), type: "send_message", alias: "master", payload },
      op.signal,
    );
    op.assertCurrent();
    if (result === null) return null;
    if (result.status === 401) notifyPrivateSessionRequired(result.status, result.body, op);
    return result;
  });
}
