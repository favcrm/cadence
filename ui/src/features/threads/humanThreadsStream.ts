import { onPrivateSessionReset, privateReadsAllowed } from "../../lib/privateSession";
import { subscribeSse, type SseOptions, type SseSubscription } from "../../lib/sse";

export type HumanThreadsScope = "rooms" | "room" | "notifications";

export type HumanThreadsPushMessage =
  | { kind: "event"; scopes: readonly HumanThreadsScope[]; room?: string }
  | { kind: "open"; reconnected: boolean }
  | { kind: "degraded" };

/** Reconciliation polling cadence when `/api/human-threads/stream` is connected. */
export const HUMAN_LIVE_POLL_MS = 60_000;
/** Polling cadence on legacy/degraded backends without SSE push. */
export const HUMAN_POLL_MS = 20_000;

const VALID_SCOPES = new Set<string>(["rooms", "room", "notifications"]);

export function parseHumanThreadsFrame(
  raw: string,
): { scopes: HumanThreadsScope[]; room?: string } | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
  const record = parsed as Record<string, unknown>;
  if (!Array.isArray(record.scopes) || record.scopes.length === 0) return null;
  const scopes: HumanThreadsScope[] = [];
  for (const item of record.scopes) {
    if (typeof item !== "string" || !VALID_SCOPES.has(item)) return null;
    const scope = item as HumanThreadsScope;
    if (!scopes.includes(scope)) scopes.push(scope);
  }
  const room =
    typeof record.room === "string" && record.room.length > 0 ? record.room : undefined;
  return { scopes, ...(room !== undefined ? { room } : {}) };
}

export interface HumanThreadsStreamStore {
  subscribe: (listener: (msg: HumanThreadsPushMessage) => void) => () => void;
  isLive: () => boolean;
  reset: () => void;
}

export function createHumanThreadsStreamStore(
  openSse: (opts: SseOptions) => SseSubscription = subscribeSse,
): HumanThreadsStreamStore {
  const listeners = new Set<(msg: HumanThreadsPushMessage) => void>();
  let sub: SseSubscription | null = null;
  let status: "idle" | "live" | "degraded" | "stopped" = "idle";
  let openedOnce = false;

  const emit = (msg: HumanThreadsPushMessage) => {
    for (const listener of [...listeners]) listener(msg);
  };

  const closeStream = () => {
    sub?.close();
    sub = null;
  };

  const reset = () => {
    closeStream();
    status = "idle";
    openedOnce = false;
  };

  const ensureConnected = () => {
    if (sub !== null || status === "stopped" || listeners.size === 0 || !privateReadsAllowed()) {
      return;
    }
    sub = openSse({
      url: "/api/human-threads/stream",
      events: ["human_threads"],
      retryMs: 1_500,
      maxRetryMs: 30_000,
      onOpen: () => {
        const reconnected = openedOnce;
        openedOnce = true;
        status = "live";
        emit({ kind: "open", reconnected });
      },
      onEvent: (event) => {
        const frame = parseHumanThreadsFrame(event.data);
        if (!frame) return;
        emit({ kind: "event", scopes: frame.scopes, ...(frame.room ? { room: frame.room } : {}) });
      },
      onError: (state) => {
        if (state === "stopped") {
          status = "stopped";
          closeStream();
          emit({ kind: "degraded" });
          return;
        }
        status = "degraded";
        emit({ kind: "degraded" });
      },
    });
  };

  return {
    subscribe(listener) {
      listeners.add(listener);
      ensureConnected();
      return () => {
        listeners.delete(listener);
        if (listeners.size === 0) {
          closeStream();
          if (status !== "stopped") status = "idle";
        }
      };
    },
    isLive() {
      return status === "live";
    },
    reset,
  };
}

const defaultStore = createHumanThreadsStreamStore();
onPrivateSessionReset(() => defaultStore.reset());

export function subscribeHumanThreadsStream(
  listener: (msg: HumanThreadsPushMessage) => void,
): () => void {
  return defaultStore.subscribe(listener);
}

export function isHumanThreadsStreamLive(): boolean {
  return defaultStore.isLive();
}
