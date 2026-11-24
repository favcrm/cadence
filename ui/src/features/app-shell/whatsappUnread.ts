import { useSyncExternalStore } from "react";
import {
  onPrivateSessionReset,
  privateReadsAllowed,
  withPrivateOperation,
} from "../../lib/privateSession";
import { sessionHeaders } from "../../lib/sessionKey";

export type WhatsAppUnreadMediaKind = "image" | "video" | "audio" | "document";

export interface WhatsAppUnreadLatest {
  conversationId: string;
  label: string | null;
  preview: string | null;
  mediaKind: WhatsAppUnreadMediaKind | null;
  fileName: string | null;
  ptt: boolean;
  lastInboundAt: string | null;
  unreadCount: number;
}

export interface WhatsAppUnreadSnapshot {
  provider: "linked" | "official" | null;
  available: boolean;
  totalUnread: number;
  unreadThreads: number;
  latest: WhatsAppUnreadLatest | null;
}

export interface ActiveWhatsAppView {
  installId: string;
  conversationId: string | null;
}

const INSTALL_ID = /^[A-Za-z0-9_-]{1,128}$/;
const MEDIA_KINDS = new Set<WhatsAppUnreadMediaKind>(["image", "video", "audio", "document"]);

const snapshots = new Map<string, WhatsAppUnreadSnapshot>();
let snapshotCache: ReadonlyMap<string, WhatsAppUnreadSnapshot> = new Map();
const listeners = new Set<() => void>();
const unsupportedInstalls = new Set<string>();
const refreshHandlers = new Map<string, Set<() => void>>();
let activeView: ActiveWhatsAppView | null = null;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function nonNegInt(value: unknown): number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : 0;
}

function nullableStr(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function emit(): void {
  snapshotCache = new Map(snapshots);
  for (const listener of listeners) listener();
}

onPrivateSessionReset(() => {
  snapshots.clear();
  unsupportedInstalls.clear();
  activeView = null;
  emit();
});

export function setActiveWhatsAppView(view: ActiveWhatsAppView | null): void {
  if (
    activeView?.installId === view?.installId &&
    activeView?.conversationId === view?.conversationId
  ) {
    return;
  }
  activeView = view;
}

export function getActiveWhatsAppView(): ActiveWhatsAppView | null {
  return activeView;
}

export function isWhatsAppUnreadUnsupported(installId: string): boolean {
  return unsupportedInstalls.has(installId);
}

export function setWhatsAppUnreadSnapshot(installId: string, next: WhatsAppUnreadSnapshot): void {
  const prev = snapshots.get(installId);
  if (prev && JSON.stringify(prev) === JSON.stringify(next)) return;
  snapshots.set(installId, next);
  emit();
}

export function clearWhatsAppUnreadSnapshot(installId: string): void {
  if (!snapshots.delete(installId)) return;
  emit();
}

export function useWhatsAppUnread(): ReadonlyMap<string, WhatsAppUnreadSnapshot> {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => snapshotCache,
    () => snapshotCache,
  );
}

export function totalWhatsAppUnread(map: ReadonlyMap<string, WhatsAppUnreadSnapshot>): number {
  let total = 0;
  for (const entry of map.values()) {
    if (entry.available && entry.totalUnread > 0) total += entry.totalUnread;
  }
  return total;
}

export function registerWhatsAppUnreadRefresh(installId: string, handler: () => void): () => void {
  let set = refreshHandlers.get(installId);
  if (!set) {
    set = new Set();
    refreshHandlers.set(installId, set);
  }
  set.add(handler);
  return () => {
    set?.delete(handler);
    if (set?.size === 0) refreshHandlers.delete(installId);
  };
}

export function requestWhatsAppUnreadRefresh(installId: string): void {
  const set = refreshHandlers.get(installId);
  if (!set) return;
  for (const handler of set) handler();
}

export function parseWhatsAppUnreadSnapshot(raw: unknown): WhatsAppUnreadSnapshot | null {
  if (!isRecord(raw)) return null;
  const provider =
    raw.provider === "linked" || raw.provider === "official" || raw.provider === null
      ? raw.provider
      : null;
  if (typeof raw.available !== "boolean") return null;
  const rawLatest = isRecord(raw.latest) ? raw.latest : null;
  const latestConversationId = rawLatest ? nullableStr(rawLatest.conversation_id) : null;
  const rawMediaKind = rawLatest ? nullableStr(rawLatest.media_kind) : null;
  const mediaKind =
    rawMediaKind && MEDIA_KINDS.has(rawMediaKind as WhatsAppUnreadMediaKind)
      ? (rawMediaKind as WhatsAppUnreadMediaKind)
      : null;
  return {
    provider,
    available: raw.available,
    totalUnread: nonNegInt(raw.total_unread),
    unreadThreads: nonNegInt(raw.unread_threads),
    latest:
      rawLatest && latestConversationId
        ? {
            conversationId: latestConversationId,
            label: nullableStr(rawLatest.label),
            preview: nullableStr(rawLatest.preview),
            mediaKind,
            fileName: nullableStr(rawLatest.file_name),
            ptt: rawLatest.ptt === true,
            lastInboundAt: nullableStr(rawLatest.last_inbound_at),
            unreadCount: nonNegInt(rawLatest.unread_count),
          }
        : null,
  };
}

export function shouldNotifyWhatsAppArrival(
  prev: WhatsAppUnreadSnapshot | null,
  next: WhatsAppUnreadSnapshot,
  view: ActiveWhatsAppView | null,
  installId: string,
  documentVisible: boolean,
): boolean {
  if (prev === null || !next.available || next.totalUnread <= 0 || !next.latest) return false;
  if (documentVisible && view?.installId === installId && view.conversationId === next.latest.conversationId) {
    return false;
  }
  const prevMs = prev.latest?.lastInboundAt ? Date.parse(prev.latest.lastInboundAt) : NaN;
  const nextMs = next.latest.lastInboundAt ? Date.parse(next.latest.lastInboundAt) : NaN;
  if (Number.isFinite(nextMs) && (!Number.isFinite(prevMs) || nextMs > prevMs)) {
    return true;
  }
  if (next.totalUnread > prev.totalUnread) return true;
  if (
    next.latest.lastInboundAt !== null &&
    next.latest.lastInboundAt !== prev.latest?.lastInboundAt &&
    next.totalUnread >= prev.totalUnread
  ) {
    return true;
  }
  return false;
}

export function formatWhatsAppUnreadNote(
  latest: WhatsAppUnreadLatest,
  t: (english: string) => string,
): string {
  const clamp = (value: string) => (value.length > 117 ? `${value.slice(0, 117)}…` : value);
  if (latest.mediaKind === "image") return clamp(`📷 ${latest.preview || t("Photo")}`);
  if (latest.mediaKind === "video") return clamp(`🎥 ${latest.preview || t("Video")}`);
  if (latest.mediaKind === "audio") {
    return latest.ptt ? `🎤 ${t("Voice message")}` : `🎵 ${t("Audio")}`;
  }
  if (latest.mediaKind === "document") {
    return clamp(`📄 ${latest.fileName || latest.preview || t("Document")}`);
  }
  return clamp(latest.preview || t("New message"));
}

export async function fetchWhatsAppUnread(
  installId: string,
  signal?: AbortSignal,
): Promise<{ ok: true; data: WhatsAppUnreadSnapshot } | { ok: false; unsupported?: boolean }> {
  if (!INSTALL_ID.test(installId) || unsupportedInstalls.has(installId) || !privateReadsAllowed()) {
    return { ok: false, unsupported: unsupportedInstalls.has(installId) };
  }
  try {
    return await withPrivateOperation(async (op) => {
      const res = await op.fetch(`/api/apps/${encodeURIComponent(installId)}/whatsapp-unread`, {
        method: "GET",
        cache: "no-store",
      });
      if (res.status === 404 || res.status === 501) {
        unsupportedInstalls.add(installId);
        clearWhatsAppUnreadSnapshot(installId);
        return { ok: false as const, unsupported: true };
      }
      if (!res.ok) return { ok: false as const };
      const raw: unknown = await res.json();
      const parsed = parseWhatsAppUnreadSnapshot(raw);
      if (!parsed) return { ok: false as const };
      return { ok: true as const, data: parsed };
    }, signal);
  } catch {
    return { ok: false };
  }
}

export async function markWhatsAppThreadUnread(
  installId: string,
  provider: "linked" | "official",
  conversationId: string,
  unread: boolean,
): Promise<{ ok: boolean; unsupported?: boolean; unreadCount?: number | null }> {
  if (!INSTALL_ID.test(installId) || unsupportedInstalls.has(installId) || !privateReadsAllowed()) {
    return { ok: false, unsupported: unsupportedInstalls.has(installId) };
  }
  try {
    const result = await withPrivateOperation(async (op) => {
      const res = await op.fetch(`/api/apps/${encodeURIComponent(installId)}/whatsapp-unread/read`, {
        method: "POST",
        headers: { ...sessionHeaders(), "content-type": "application/json" },
        body: JSON.stringify({ provider, conversation_id: conversationId, unread }),
        cache: "no-store",
      });
      if (res.status === 404 || res.status === 501) {
        // Distinguish route-missing 404 on older workers from conversation_not_found JSON error.
        const body = await res.json().catch(() => null);
        if (!isRecord(body) || body.error === undefined) {
          unsupportedInstalls.add(installId);
        }
        return { ok: false, unsupported: !isRecord(body) || body.error === undefined };
      }
      if (!res.ok) return { ok: false };
      const body = await res.json().catch(() => null);
      const unreadCount =
        isRecord(body) && typeof body.unread_count === "number" ? body.unread_count : null;
      return { ok: true, unreadCount };
    });
    if (result.ok) requestWhatsAppUnreadRefresh(installId);
    return result;
  } catch {
    return { ok: false };
  }
}
