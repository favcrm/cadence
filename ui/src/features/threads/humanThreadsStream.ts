import { useCallback, useSyncExternalStore } from "react";
import { onPrivateSessionReset, privateReadsAllowed } from "../../lib/privateSession";
import { subscribeSse, type SseOptions, type SseSubscription } from "../../lib/sse";
import { sanitizePreviewText } from "../app-shell/chat/conversationPreview";

export type HumanThreadsScope = "rooms" | "room" | "notifications";
export type HumanThreadsPreviewState = "queued" | "working" | "streaming" | "clear";

export interface HumanThreadsPreviewFrame {
  room: string;
  agent_id: string;
  alias: string;
  state: HumanThreadsPreviewState;
  text: string | null;
  step?: string;
  started_at?: number;
}

export interface ChannelLivePreview {
  agent_id: string;
  alias: string;
  state: "queued" | "working" | "streaming";
  text: string | null;
  step?: string;
  started_at?: number;
}

export type HumanThreadsPushMessage =
  | { kind: "event"; scopes: readonly HumanThreadsScope[]; room?: string }
  | { kind: "open"; reconnected: boolean }
  | { kind: "degraded" };

/** Reconciliation polling cadence when `/api/human-threads/stream` is connected. */
export const HUMAN_LIVE_POLL_MS = 60_000;
/** Polling cadence on legacy/degraded backends without SSE push. */
export const HUMAN_POLL_MS = 20_000;

const VALID_SCOPES = new Set<string>(["rooms", "room", "notifications"]);
const VALID_PREVIEW_STATES = new Set<string>(["queued", "working", "streaming", "clear"]);
const EMPTY_PREVIEWS: readonly ChannelLivePreview[] = [];

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

export function parseHumanThreadsPreviewFrame(raw: string): HumanThreadsPreviewFrame | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
  const rec = parsed as Record<string, unknown>;
  if (
    typeof rec.room !== "string" ||
    rec.room.length === 0 ||
    typeof rec.agent_id !== "string" ||
    rec.agent_id.length === 0 ||
    typeof rec.alias !== "string" ||
    rec.alias.length === 0 ||
    typeof rec.state !== "string" ||
    !VALID_PREVIEW_STATES.has(rec.state)
  ) {
    return null;
  }
  if (rec.text !== null && typeof rec.text !== "string") return null;
  const step = typeof rec.step === "string" && rec.step.length > 0 ? rec.step.slice(0, 120) : undefined;
  const startedAt =
    typeof rec.started_at === "number" && Number.isFinite(rec.started_at) && rec.started_at >= 0
      ? rec.started_at
      : undefined;
  return {
    room: rec.room,
    agent_id: rec.agent_id,
    alias: rec.alias,
    state: rec.state as HumanThreadsPreviewState,
    text: rec.text === null ? null : sanitizePreviewText(rec.text),
    ...(step !== undefined ? { step } : {}),
    ...(startedAt !== undefined ? { started_at: startedAt } : {}),
  };
}

export interface HumanThreadsStreamStore {
  subscribe: (listener: (msg: HumanThreadsPushMessage) => void) => () => void;
  subscribePreviews: (listener: () => void) => () => void;
  previewsForRoom: (roomId: string | null) => readonly ChannelLivePreview[];
  applyPreviewFrame: (frame: HumanThreadsPreviewFrame) => void;
  clearRoomPreviews: (roomId: string) => void;
  isLive: () => boolean;
  reset: () => void;
}

export function createHumanThreadsStreamStore(
  openSse: (opts: SseOptions) => SseSubscription = subscribeSse,
): HumanThreadsStreamStore {
  const listeners = new Set<(msg: HumanThreadsPushMessage) => void>();
  const previewListeners = new Set<() => void>();
  const previewsByRoom = new Map<string, Map<string, ChannelLivePreview>>();
  const roomSnapshots = new Map<string, readonly ChannelLivePreview[]>();
  let sub: SseSubscription | null = null;
  let status: "idle" | "live" | "degraded" | "stopped" = "idle";
  let openedOnce = false;
  let previewNotifyScheduled = false;

  const emit = (msg: HumanThreadsPushMessage) => {
    for (const listener of [...listeners]) listener(msg);
  };

  const notifyPreviews = () => {
    if (previewNotifyScheduled || previewListeners.size === 0) return;
    previewNotifyScheduled = true;
    const flush = () => {
      previewNotifyScheduled = false;
      for (const listener of [...previewListeners]) listener();
    };
    if (typeof requestAnimationFrame === "function") requestAnimationFrame(flush);
    else setTimeout(flush, 16);
  };

  const clearAllPreviews = () => {
    if (previewsByRoom.size === 0) return;
    previewsByRoom.clear();
    roomSnapshots.clear();
    notifyPreviews();
  };

  const clearRoomPreviews = (roomId: string) => {
    if (!previewsByRoom.delete(roomId)) return;
    roomSnapshots.delete(roomId);
    notifyPreviews();
  };

  const applyPreviewFrame = (frame: HumanThreadsPreviewFrame) => {
    let roomMap = previewsByRoom.get(frame.room);
    if (frame.state === "clear") {
      if (!roomMap || !roomMap.delete(frame.agent_id)) return;
      if (roomMap.size === 0) previewsByRoom.delete(frame.room);
      roomSnapshots.delete(frame.room);
      notifyPreviews();
      return;
    }
    if (!roomMap) {
      roomMap = new Map();
      previewsByRoom.set(frame.room, roomMap);
    }
    const prev = roomMap.get(frame.agent_id);
    const nextText =
      frame.text !== null
        ? frame.text
        : frame.state === "working" || frame.state === "queued"
          ? null
          : (prev?.text ?? null);
    roomMap.set(frame.agent_id, {
      agent_id: frame.agent_id,
      alias: frame.alias,
      state: frame.state,
      text: nextText,
      ...(frame.step !== undefined ? { step: frame.step } : prev?.step ? { step: prev.step } : {}),
      ...(frame.started_at !== undefined
        ? { started_at: frame.started_at }
        : prev?.started_at !== undefined
          ? { started_at: prev.started_at }
          : {}),
    });
    roomSnapshots.delete(frame.room);
    notifyPreviews();
  };

  const previewsForRoom = (roomId: string | null): readonly ChannelLivePreview[] => {
    if (!roomId) return EMPTY_PREVIEWS;
    const cached = roomSnapshots.get(roomId);
    if (cached) return cached;
    const roomMap = previewsByRoom.get(roomId);
    if (!roomMap || roomMap.size === 0) return EMPTY_PREVIEWS;
    const snapshot = [...roomMap.values()];
    roomSnapshots.set(roomId, snapshot);
    return snapshot;
  };

  const closeStream = () => {
    sub?.close();
    sub = null;
  };

  const reset = () => {
    closeStream();
    clearAllPreviews();
    status = "idle";
    openedOnce = false;
  };

  const ensureConnected = () => {
    if (sub !== null || status === "stopped" || listeners.size === 0 || !privateReadsAllowed()) {
      return;
    }
    sub = openSse({
      url: "/api/human-threads/stream",
      events: ["human_threads", "human_threads_preview"],
      retryMs: 1_500,
      maxRetryMs: 30_000,
      onOpen: () => {
        const reconnected = openedOnce;
        openedOnce = true;
        status = "live";
        emit({ kind: "open", reconnected });
      },
      onEvent: (event) => {
        if (event.type === "human_threads_preview") {
          const preview = parseHumanThreadsPreviewFrame(event.data);
          if (preview) applyPreviewFrame(preview);
          return;
        }
        const frame = parseHumanThreadsFrame(event.data);
        if (!frame) return;
        emit({ kind: "event", scopes: frame.scopes, ...(frame.room ? { room: frame.room } : {}) });
      },
      onError: (state) => {
        if (state === "stopped") {
          status = "stopped";
          closeStream();
          clearAllPreviews();
          emit({ kind: "degraded" });
          return;
        }
        status = "degraded";
        clearAllPreviews();
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
          clearAllPreviews();
          if (status !== "stopped") status = "idle";
        }
      };
    },
    subscribePreviews(listener) {
      previewListeners.add(listener);
      return () => {
        previewListeners.delete(listener);
      };
    },
    previewsForRoom,
    applyPreviewFrame,
    clearRoomPreviews,
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

export function clearChannelRoomPreviews(roomId: string): void {
  defaultStore.clearRoomPreviews(roomId);
}

const serverPreviewSnapshot = (): readonly ChannelLivePreview[] => EMPTY_PREVIEWS;

export function useChannelRoomPreviews(roomId: string | null): readonly ChannelLivePreview[] {
  const getSnapshot = useCallback(() => defaultStore.previewsForRoom(roomId), [roomId]);
  return useSyncExternalStore(defaultStore.subscribePreviews, getSnapshot, serverPreviewSnapshot);
}
