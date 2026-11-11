import { ApiError, errorMessage } from "./apiError";
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

const CLOSED = 2;
const PING_INTERVAL_MS = 25_000;
const COMMAND_TIMEOUT_MS = 15_000;
const UNSUPPORTED_COOLDOWN_MS = 15_000;
const IDLE_CLOSE_GRACE_MS = 50;
const MAX_CLIENT_FRAME_BYTES = 38_000;

type EntryGlobal = typeof globalThis & {
  __CADENCE_ENTRY_MODE__?: boolean;
};

type SocketTarget =
  | {
      kind: "board";
      entities: boolean;
      acceptDegraded: boolean;
    }
  | {
      kind: "thread";
      alias: "master";
      conversation?: string;
      after?: number;
    };

type ServerFrame =
  | { v: 1; type: "welcome"; build: string }
  | { v: 1; id?: string; type: "stream_open"; subId: string }
  | { v: 1; type: "stream_event"; subId: string; event: string; data: string; id?: string }
  | {
      v: 1;
      id?: string;
      type: "stream_error";
      subId: string;
      status: number;
      code: string;
      message?: string;
    }
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
  onStreamError(status: number, code: string, message?: string): void;
  onStreamClosed(): void;
  onFallbackToHttp(): void;
}

interface PendingCommand {
  readonly id: string;
  resolve(result: { status: number; body: unknown } | null): void;
  cleanup(): void;
}

type TimerHandle = ReturnType<typeof setTimeout>;

function unrefTimer(timer: unknown): void {
  if (typeof timer === "object" && timer !== null && "unref" in timer) {
    const fn = Reflect.get(timer, "unref");
    if (typeof fn === "function") fn.call(timer);
  }
}

function entryMode(): boolean {
  try {
    return (globalThis as EntryGlobal).__CADENCE_ENTRY_MODE__ === true;
  } catch {
    return false;
  }
}

let unsupportedUntil = 0;
let nextSeq = 0;

function nextId(prefix: string): string {
  nextSeq += 1;
  return `${prefix}_${nextSeq}`;
}

/**
 * Hosted boards authenticate with an HttpOnly same-origin cookie (`sessionKey() === null`),
 * whereas the local native daemon requires `X-Cadence-Session` on HTTP requests (which
 * browser `new WebSocket` cannot attach) and credential-entry documents run under
 * `__CADENCE_ENTRY_MODE__`. Only hosted cookie-authenticated browser tabs open `/api/ws`.
 */
export function canUseBoardSocket(): boolean {
  if (typeof window === "undefined" || typeof WebSocket !== "function") return false;
  if (entryMode()) return false;
  if (sessionKey() !== null) return false;
  if (!privateReadsAllowed()) return false;
  const proto = window.location?.protocol;
  const host = window.location?.host;
  if ((proto !== "https:" && proto !== "http:") || typeof host !== "string" || host.length === 0) {
    return false;
  }
  return Date.now() >= unsupportedUntil;
}

function socketUrl(): string {
  const scheme = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${window.location.host}/api/ws`;
}

export function parseBoardSocketTarget(url: string): SocketTarget | null {
  if (!url.startsWith("/") || url.startsWith("//")) return null;
  const [path, query = ""] = url.split("?", 2);
  const params = new URLSearchParams(query);

  if (path === "/api/stream") {
    for (const key of params.keys()) {
      if (key !== "entities" && key !== "accept_degraded" && key !== "after") return null;
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
    const rawAfter = params.get("after");
    let after: number | undefined;
    if (rawAfter !== null && rawAfter !== "") {
      if (!/^\d+$/.test(rawAfter)) return null;
      const parsed = Number(rawAfter);
      if (!Number.isSafeInteger(parsed) || parsed < 0) return null;
      after = parsed;
    }
    const conversation = params.get("conversation");
    return {
      kind: "thread",
      alias: "master",
      ...(conversation !== null && conversation !== "" ? { conversation } : {}),
      ...(after !== undefined ? { after } : {}),
    };
  }

  return null;
}

function parseServerFrame(raw: unknown): ServerFrame | null {
  if (typeof raw !== "string" || raw === "pong") return null;
  let decoded: unknown;
  try {
    decoded = JSON.parse(raw);
  } catch {
    return null;
  }
  if (decoded === null || typeof decoded !== "object" || Array.isArray(decoded)) return null;
  const record = decoded as Record<string, unknown>;
  if (record.v !== 1 || typeof record.type !== "string") return null;
  return record as unknown as ServerFrame;
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
    this.ws = new WebSocket(socketUrl());
    this.ws.addEventListener("message", (event) => this.handleMessage(event.data));
    this.ws.addEventListener("error", () => {
      // Browser WebSockets always follow `error` with `close`, but if an
      // environment stalls before `close` while still CONNECTING, tear down cleanly.
      if (!this.welcomed && this.ws.readyState === CLOSED + 1) {
        this.teardown("socket_error");
      }
    });
    this.ws.addEventListener("close", (event) => {
      if (event.code === 4001 && this.generation === privateSessionGeneration() && typeof window !== "undefined") {
        window.dispatchEvent(new Event("cadence:session-required"));
      }
      this.teardown("socket_closed");
    });
  }

  isReady(): boolean {
    return !this.closed && this.welcomed && this.ws.readyState === 1 && this.generation === privateSessionGeneration();
  }

  addSubscription(sub: ActiveSubscription): void {
    this.clearIdleTimer();
    this.subscriptions.set(sub.subId, sub);
    if (this.isReady()) {
      this.sendSubscribe(sub);
    }
  }

  removeSubscription(subId: string, notifyServer: boolean): void {
    const existing = this.subscriptions.get(subId);
    if (!existing) return;
    this.subscriptions.delete(subId);
    if (notifyServer && existing.subscribeSent && this.isReady()) {
      this.sendJson({
        v: 1,
        id: nextId("unsub"),
        type: "unsubscribe",
        subId,
      });
    }
    this.scheduleIdleCheck();
  }

  sendCommand(
    frame: Record<string, unknown> & { id: string },
    signal: AbortSignal,
  ): Promise<{ status: number; body: unknown } | null> {
    if (!this.isReady()) return Promise.resolve(null);
    const encoded = JSON.stringify(frame);
    if (new TextEncoder().encode(encoded).byteLength > MAX_CLIENT_FRAME_BYTES) {
      return Promise.resolve(null);
    }
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
      this.commands.set(frame.id, { id: frame.id, resolve: finish, cleanup });
      try {
        this.ws.send(encoded);
      } catch {
        finish(null);
      }
    });
  }

  private sendSubscribe(sub: ActiveSubscription): void {
    if (sub.subscribeSent || !this.isReady()) return;
    sub.subscribeSent = true;
    const id = nextId("sub");
    const sent =
      sub.target.kind === "board"
        ? this.sendJson({
            v: 1,
            id,
            subId: sub.subId,
            type: "subscribe_board",
            entities: sub.target.entities,
            acceptDegraded: sub.target.acceptDegraded,
          })
        : this.sendJson({
            v: 1,
            id,
            subId: sub.subId,
            type: "subscribe_thread",
            alias: sub.target.alias,
            ...(sub.target.conversation !== undefined ? { conversation: sub.target.conversation } : {}),
            ...(sub.target.after !== undefined ? { after: sub.target.after } : {}),
          });
    if (!sent) {
      this.subscriptions.delete(sub.subId);
      sub.onStreamClosed();
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
        // Answered directly at Cloudflare Edge via `setWebSocketAutoResponse("ping", "pong")`
        // with zero Durable Object wakeups while idle.
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
      if (this.subscriptions.size === 0 && this.commands.size === 0) {
        this.teardown("idle");
      }
    }, IDLE_CLOSE_GRACE_MS);
    unrefTimer(timer);
    this.idleTimer = timer;
  }

  private handleMessage(raw: unknown): void {
    if (this.closed || this.generation !== privateSessionGeneration()) return;
    const frame = parseServerFrame(raw);
    if (!frame) return;

    switch (frame.type) {
      case "welcome": {
        if (this.welcomed) return;
        this.welcomed = true;
        unsupportedUntil = 0;
        this.startPing();
        for (const sub of [...this.subscriptions.values()]) {
          this.sendSubscribe(sub);
        }
        return;
      }
      case "stream_open": {
        this.subscriptions.get(frame.subId)?.onOpen();
        return;
      }
      case "stream_event": {
        this.subscriptions.get(frame.subId)?.onEvent(frame.event, frame.data, frame.id);
        return;
      }
      case "stream_error": {
        const sub = this.subscriptions.get(frame.subId);
        if (!sub) return;
        this.subscriptions.delete(frame.subId);
        sub.onStreamError(frame.status, frame.code, frame.message);
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
      case "command_result": {
        const pending = this.commands.get(frame.id);
        if (!pending) return;
        pending.resolve({ status: frame.status, body: frame.body });
        return;
      }
      case "revoked": {
        if (typeof window !== "undefined") {
          window.dispatchEvent(new Event("cadence:session-required"));
        }
        this.teardown("revoked");
        return;
      }
      case "error": {
        if (frame.id) {
          const pending = this.commands.get(frame.id);
          if (pending) {
            pending.resolve({
              status: 400,
              body: { error: { code: frame.code, message: frame.message ?? frame.code } },
            });
          }
        }
        return;
      }
    }
  }

  teardown(reason: "socket_closed" | "socket_error" | "session_paused" | "revoked" | "idle" | "reset"): void {
    if (this.closed) return;
    this.closed = true;
    if (activeSocket === this) {
      activeSocket = null;
    }
    this.stopPing();
    this.clearIdleTimer();

    const failedBeforeWelcome = !this.welcomed && (reason === "socket_closed" || reason === "socket_error");
    if (failedBeforeWelcome) {
      unsupportedUntil = Date.now() + UNSUPPORTED_COOLDOWN_MS;
    }

    const subs = [...this.subscriptions.values()];
    this.subscriptions.clear();
    const cmds = [...this.commands.values()];
    this.commands.clear();

    for (const cmd of cmds) {
      cmd.cleanup();
      cmd.resolve(null);
    }

    for (const sub of subs) {
      if (failedBeforeWelcome) {
        sub.onFallbackToHttp();
      } else {
        sub.onStreamClosed();
      }
    }

    try {
      if (this.ws.readyState === 0 || this.ws.readyState === 1) {
        this.ws.close(1000, reason);
      }
    } catch {
      // Ignore already-closing socket errors.
    }
  }
}

let activeSocket: SharedBoardSocket | null = null;
let pauseRegistered = false;

function ensurePauseHook(): void {
  if (pauseRegistered) return;
  pauseRegistered = true;
  onPrivateReadsPaused(() => {
    activeSocket?.teardown("session_paused");
  });
}

function getOrCreateSocket(): SharedBoardSocket {
  ensurePauseHook();
  if (
    activeSocket !== null &&
    !activeSocket.closed &&
    activeSocket.generation === privateSessionGeneration() &&
    (activeSocket.ws.readyState === 0 || activeSocket.ws.readyState === 1)
  ) {
    return activeSocket;
  }
  activeSocket?.teardown("reset");
  activeSocket = new SharedBoardSocket();
  return activeSocket;
}

/**
 * Multiplex `/api/stream` and `/api/threads/master/stream` onto the tab's single
 * hibernatable Durable Object WebSocket (`/api/ws`), falling back transparently
 * to `fallback(url)` (`fetch` SSE) on local header-authenticated daemons, non-master
 * streams, or older Worker builds before `/api/ws` is live.
 */
export function openBoardSocketStream(
  url: string,
  fallback: (url: string) => EventSourceLike,
): EventSourceLike {
  const target = parseBoardSocketTarget(url);
  if (target === null || !canUseBoardSocket()) {
    return fallback(url);
  }

  const generation = privateSessionGeneration();
  let operation: PrivateOperation | null = null;
  let stopFollow = () => {};
  const abortController = new AbortController();
  if (privateReadsAllowed()) {
    try {
      operation = beginPrivateOperation();
      stopFollow = bindPrivateAbort(abortController, operation.signal);
    } catch {
      operation = null;
    }
  }

  if (!operation || abortController.signal.aborted) {
    stopFollow();
    operation?.dispose();
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
    Object.assign(source, { readyState: CLOSED });
    source.onerror?.(new Event("error"));
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
    close() {
      if (closed) return;
      closed = true;
      cleanupOperation();
      if (delegate) {
        delegate.close();
        delegate = null;
      } else {
        socket.removeSubscription(subId, true);
      }
      Object.assign(source, { readyState: CLOSED });
    },
  };

  abortController.signal.addEventListener(
    "abort",
    () => {
      if (closed) return;
      closed = true;
      cleanupOperation();
      if (delegate) {
        delegate.close();
        delegate = null;
      } else {
        socket.removeSubscription(subId, false);
      }
      Object.assign(source, { readyState: CLOSED });
    },
    { once: true },
  );

  const sub: ActiveSubscription = {
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
      Object.assign(source, { readyState: 1 });
      source.onopen?.(new Event("open"));
    },
    onEvent(eventType, data, id) {
      if (closed || !operation || generation !== privateSessionGeneration()) return;
      if (id !== undefined && !id.includes("\0")) {
        lastId = id;
      }
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
      if (operation && status === 401) {
        notifyPrivateSessionRequired(status, { code }, operation);
      }
      failClosed();
    },
    onStreamClosed() {
      failClosed();
    },
    onFallbackToHttp() {
      if (closed || !privateReadsAllowed() || generation !== privateSessionGeneration()) {
        failClosed();
        return;
      }
      // Dispose the socket-bound operation before delegating so `fallback(url)`
      // owns a single fresh `PrivateOperation`.
      cleanupOperation();
      const httpSource = fallback(url);
      delegate = httpSource;
      Object.assign(source, { readyState: httpSource.readyState });
      for (const [type, set] of listeners.entries()) {
        for (const listener of set) {
          httpSource.addEventListener(type, listener);
        }
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
  };

  socket.addSubscription(sub);
  return source;
}

/**
 * Submit a `master` thread message over the already-open board WebSocket (`send_message`),
 * avoiding the HTTP/TLS/Worker/Central-D1/DO cold-request chain (~5x lower latency).
 * Returns `{ handled: false }` whenever the socket is not ready or drops mid-flight so
 * `api.threadSend` falls back seamlessly to `POST /api/threads/master/messages` using
 * the same idempotent `message` key.
 */
export async function sendMasterMessageViaSocket<T>(
  alias: string,
  payload: Record<string, unknown>,
  operation?: PrivateOperation,
): Promise<{ handled: true; value: T } | { handled: false }> {
  if (alias !== "master" || !canUseBoardSocket()) {
    return { handled: false };
  }
  const socket = activeSocket;
  if (socket === null || !socket.isReady()) {
    return { handled: false };
  }

  return usingPrivateOperation(operation, async (op) => {
    if (!socket.isReady()) return { handled: false };
    const id = nextId("msg");
    const result = await socket.sendCommand(
      {
        v: 1,
        id,
        type: "send_message",
        alias: "master",
        payload,
      },
      op.signal,
    );
    op.assertCurrent();
    if (result === null) {
      return { handled: false };
    }
    const { status, body } = result;
    if (status === 401) {
      notifyPrivateSessionRequired(status, body, op);
    }
    if (status < 200 || status >= 300) {
      const record =
        body !== null && typeof body === "object" && !Array.isArray(body)
          ? (body as Record<string, unknown>)
          : null;
      const nestedError =
        record && typeof record.error === "object" && record.error !== null && !Array.isArray(record.error)
          ? (record.error as Record<string, unknown>)
          : null;
      const nestedCode =
        typeof record?.code === "string"
          ? record.code
          : typeof nestedError?.code === "string"
            ? nestedError.code
            : undefined;
      throw new ApiError(
        errorMessage(body, `${status}`),
        status,
        record ? { ...record, ...(nestedCode === undefined ? {} : { code: nestedCode }) } : undefined,
      );
    }
    return { handled: true, value: body as T };
  });
}

/** Deterministic teardown for unit tests. */
export function resetBoardSocketForTests(): void {
  unsupportedUntil = 0;
  activeSocket?.teardown("reset");
  activeSocket = null;
}
