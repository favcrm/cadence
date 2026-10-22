import { useEffect, useRef, useState } from "react";
import { useResource } from "../../../lib/useResource";
import { resources } from "../../../lib/resources";
import { useNotificationToast } from "../../threads/NotificationToast";
import { conversationList } from "../conversationClient";
import { setAssistantUnread } from "./assistantUnread";
import { visibleConversation, setKnownInstalls } from "./chatVisibility";
import { isUnread, readSeen } from "./unreadSeen";

/**
 * CAD-1700: one install's reply dot, observed for the shell's whole
 * lifetime. The pane cannot own this — it unmounts when the operator goes
 * to Home, Issues or another app, which is exactly when a reply lands.
 * Each probe OBSERVES its install's conversation list, so the list is
 * fetched and kept fresh by `master` frames rather than read from a cache
 * nobody opened. An install whose turn just finished on one of its
 * conversations is marked unread even before its list catches up; another
 * tab's `storage` write clears it here too.
 */
function UnreadProbe({ installId, finished }: { installId: string; finished: string | null }) {
  const [written, setWritten] = useState(0);
  const list = useResource(conversationList(installId));
  const conversations = list.data?.legacy === false ? list.data.conversations : null;

  useEffect(() => {
    if (conversations === null) return;
    const seen = readSeen(installId);
    const unread = conversations.some((c) => isUnread(seen, c.id, c.updated));
    // A finished turn counts only while that conversation is still unseen:
    // another tab opening it writes the seen map, and this must not re-raise.
    const finishedUnread =
      finished !== null && conversations.some((c) => c.id === finished && isUnread(seen, c.id, c.updated));
    setAssistantUnread(installId, unread || finishedUnread);
  }, [conversations, installId, finished, written]);

  useEffect(() => {
    const onStorage = (event: StorageEvent) => {
      if (event.key !== null && event.key.startsWith("cadence.assistant.seen")) setWritten((n) => n + 1);
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  return null;
}

/** One finished turn, as the shell saw it. */
interface FinishedTurn {
  conversation: string | null;
  install: string | null;
}

/**
 * The turn edge the shell announces on: a previous turn that is no longer
 * the running/queued one — a new message replaced it, or the queue drained.
 * Identity is the message, so a completion immediately followed by the next
 * turn is still seen.
 */
function useFinishedTurn(turn: {
  message?: string;
  conversation?: string | null;
  position?: number | null;
  app_origin?: { install_id: string } | null;
} | null): FinishedTurn | null {
  const previous = useRef<{ message: string; conversation: string | null; install: string | null; working: boolean } | null | undefined>(undefined);
  const current = turn
    ? {
        message: turn.message ?? "",
        conversation: turn.conversation ?? null,
        install: turn.app_origin?.install_id ?? null,
        // `position` is null (or omitted) only while the turn is WORKING: a
        // queued message that is cancelled or becomes the head was never a
        // reply.
        working: turn.position == null,
      }
    : null;
  const before = previous.current;
  const finished =
    before != null && before.working && (current === null || current.message !== before.message)
      ? { conversation: before.conversation, install: before.install }
      : null;
  useEffect(() => {
    previous.current = current;
  }, [current?.message, current?.conversation, current?.install, current?.working]);
  return finished;
}

/** Mounted once by the board shell: one probe per installed app. */
export function AssistantUnreadProbes({ apps }: { apps: { installId: string }[] | null }) {
  const master = useResource(resources.masterState);
  const toast = useNotificationToast();
  const finished = useFinishedTurn(master.data?.turn ?? null);
  const [announced, setAnnounced] = useState<FinishedTurn | null>(null);
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    setKnownInstalls(apps === null ? [] : apps.map((app) => app.installId));
  }, [apps]);

  useEffect(() => {
    if (finished === null) return;
    setAnnounced(finished);
    setAttempt(0);
  }, [finished]);

  useEffect(() => {
    if (announced === null || apps === null) return;
    const conversation = announced.conversation;
    if (conversation === null) {
      setAnnounced(null);
      return;
    }
    // Subject conversations (campaign, WhatsApp) carry no app origin by
    // design; their install is the one whose observed list holds them.
    const install =
      announced.install ??
      apps.find((app) => conversationList(app.installId).get().data?.conversations.some((c) => c.id === conversation))
        ?.installId ??
      null;
    if (install === null) {
      // No list has loaded yet: hold the announcement and retry, rather than
      // dropping a reply the operator has not seen.
      if (attempt < 15) {
        const timer = setTimeout(() => setAttempt((n) => n + 1), 1000);
        return () => clearTimeout(timer);
      }
      setAnnounced(null);
      return;
    }
    // The live pane selection is the only authority here: a URL query can be
    // stale after the picker moved, and must not veto a real reply.
    if (visibleConversation()?.conversation !== conversation) {
      setAssistantUnread(install, true);
      const params = new URLSearchParams({ conversation });
      toast.show("Assistant replied", `assistant:${install}:${conversation}`, {
        href: `/app-installations/${encodeURIComponent(install)}?${params.toString()}`,
        note: "An installed app's conversation",
      });
    }
    setAnnounced(null);
  }, [announced, apps, toast, attempt]);

  if (apps === null) return null;
  return (
    <>
      {apps.map((app) => (
        <UnreadProbe key={app.installId} installId={app.installId} finished={finished?.conversation ?? null} />
      ))}
    </>
  );
}
