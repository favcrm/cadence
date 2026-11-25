import { useEffect, useRef } from "react";
import { useTranslate } from "../../lib/locale";
import { privateReadsAllowed } from "../../lib/privateSession";
import { useNotificationToast } from "../threads/NotificationToast";
import { useChannelsUnread } from "../threads/channelsUnread";
import type { VerifiedApp } from "./appNav";
import { useDocumentVisible } from "./chat/chatVisibility";
import {
  clearWhatsAppUnreadSnapshot,
  formatWhatsAppUnreadNote,
  getActiveWhatsAppView,
  fetchWhatsAppUnread,
  isWhatsAppUnreadUnsupported,
  registerWhatsAppUnreadRefresh,
  setWhatsAppUnreadSnapshot,
  shouldNotifyWhatsAppArrival,
  totalWhatsAppUnread,
  useWhatsAppUnread,
  type WhatsAppUnreadSnapshot,
} from "./whatsappUnread";

const POLL_INTERVAL_MS = 15_000;

function stripTitleCount(title: string): string {
  return title.replace(/^\(\d+\)\s*/, "");
}

function WhatsAppTitleBadge() {
  const unread = useWhatsAppUnread();
  const channelsUnread = useChannelsUnread();
  const total = totalWhatsAppUnread(unread) + channelsUnread;
  const baseTitleRef = useRef<string | null>(null);

  useEffect(() => {
    if (typeof document === "undefined") return;
    if (baseTitleRef.current === null) {
      baseTitleRef.current = stripTitleCount(document.title || "cadence board");
    }
    const base = baseTitleRef.current;
    const desired = total > 0 ? `(${total}) ${base}` : base;
    if (document.title !== desired) {
      document.title = desired;
    }
  }, [total]);

  useEffect(() => {
    return () => {
      if (typeof document === "undefined" || baseTitleRef.current === null) return;
      document.title = stripTitleCount(baseTitleRef.current);
    };
  }, []);

  return null;
}

function WhatsAppUnreadProbe({
  installId,
  resolveHref,
}: {
  installId: string;
  resolveHref: (installId: string) => string | null;
}) {
  const toast = useNotificationToast();
  const t = useTranslate();
  const documentVisible = useDocumentVisible();
  const prevRef = useRef<WhatsAppUnreadSnapshot | null>(null);
  const notifiedRoomsRef = useRef<Set<string>>(new Set());
  const resolveHrefRef = useRef(resolveHref);
  resolveHrefRef.current = resolveHref;
  const tRef = useRef(t);
  tRef.current = t;

  useEffect(() => {
    let active = true;
    let inFlight: AbortController | null = null;
    let pollSeq = 0;

    const runPoll = async () => {
      if (!active || !privateReadsAllowed() || isWhatsAppUnreadUnsupported(installId)) return;
      inFlight?.abort();
      const controller = new AbortController();
      inFlight = controller;
      const seq = ++pollSeq;

      const res = await fetchWhatsAppUnread(installId, controller.signal);
      if (!active || seq !== pollSeq || !res.ok) return;

      const data = res.data;
      const prev = prevRef.current;
      prevRef.current = data;
      setWhatsAppUnreadSnapshot(installId, data);

      const visibleNow = typeof document !== "undefined" ? document.visibilityState === "visible" : true;
      const activeView = getActiveWhatsAppView();

      if (data.totalUnread === 0) {
        for (const room of notifiedRoomsRef.current) toast.clear(room);
        notifiedRoomsRef.current.clear();
        return;
      }

      if (visibleNow && activeView?.installId === installId && activeView.conversationId) {
        const currentRoom = `whatsapp:${installId}:${activeView.conversationId}`;
        if (notifiedRoomsRef.current.has(currentRoom)) {
          toast.clear(currentRoom);
          notifiedRoomsRef.current.delete(currentRoom);
        }
      }

      if (
        visibleNow &&
        data.latest &&
        shouldNotifyWhatsAppArrival(prev, data, activeView, installId, visibleNow)
      ) {
        const room = `whatsapp:${installId}:${data.latest.conversationId}`;
        const basePath =
          resolveHrefRef.current(installId) ?? `/app-installations/${encodeURIComponent(installId)}`;
        const sep = basePath.includes("?") ? "&" : "?";
        const href = `${basePath}${sep}waba=inbox&wa_thread=${encodeURIComponent(data.latest.conversationId)}`;
        const title = data.latest.label
          ? `WhatsApp · ${data.latest.label}`
          : tRef.current("New WhatsApp message");
        const note = formatWhatsAppUnreadNote(data.latest, tRef.current);
        notifiedRoomsRef.current.add(room);
        toast.show(title, room, { href, note });
      }
    };

    void runPoll();

    const trigger = () => {
      void runPoll();
    };
    const interval = documentVisible ? window.setInterval(trigger, POLL_INTERVAL_MS) : undefined;
    window.addEventListener("focus", trigger);
    const stopRefresh = registerWhatsAppUnreadRefresh(installId, trigger);

    return () => {
      active = false;
      inFlight?.abort();
      if (interval !== undefined) window.clearInterval(interval);
      window.removeEventListener("focus", trigger);
      stopRefresh();
    };
  }, [installId, documentVisible, toast]);

  useEffect(() => {
    const rooms = notifiedRoomsRef.current;
    return () => {
      clearWhatsAppUnreadSnapshot(installId);
      for (const room of rooms) toast.clear(room);
      rooms.clear();
    };
  }, [installId, toast]);

  return null;
}

/** Polls WhatsApp unread summaries for installed WABA apps, drives sidebar counts and arrival toasts. */
export function WhatsAppUnreadProbes({
  apps,
  resolveHref,
}: {
  apps: VerifiedApp[] | null;
  resolveHref: (installId: string) => string | null;
}) {
  const wabaApps = apps === null ? [] : apps.filter((app) => app.kind === "waba");
  return (
    <>
      <WhatsAppTitleBadge />
      {wabaApps.map((app) => (
        <WhatsAppUnreadProbe key={app.installId} installId={app.installId} resolveHref={resolveHref} />
      ))}
    </>
  );
}
