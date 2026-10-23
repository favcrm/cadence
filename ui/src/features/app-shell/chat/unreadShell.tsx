import { useEffect, useRef } from "react";
import { useQuery } from "../../../lib/useResource";
import { useNotificationToast } from "../../threads/NotificationToast";
import { conversationLabel, conversationList } from "../conversationClient";
import { setAssistantUnread } from "./assistantUnread";
import { visibleConversation, setKnownInstalls, useDocumentVisible } from "./chatVisibility";
import { announceReply, isUnread, onSeenChanged, readSeen, writeSeen } from "./unreadSeen";

/** Observe real reply watermarks for the shell's lifetime, even when the app
 * pane is unmounted. An operator send, tool step or empty conversation never
 * advances this watermark. List reads refresh on the existing master frames.
 *
 * Reads and reservations are asynchronous; each effect run fences the previous
 * one so a slow read cannot clear a room another run already re-observed. A
 * winning reservation is re-checked against fresh seen state before display,
 * but deliberately not against the originating run: reservations do not wake
 * observers, and unrelated commits must not silence a toast already won. */
function UnreadProbe({ installId }: { installId: string }) {
  const list = useQuery(conversationList(installId));
  const conversations = list.data?.legacy === false ? list.data.conversations : null;
  const toast = useNotificationToast();
  const documentVisible = useDocumentVisible();
  // Effect re-runs (a list refresh, a visibility change) are not unmounts:
  // the winning reservation may still display after its origin run retires.
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  useEffect(() => {
    if (conversations === null) return;
    let observing = true;
    let latest = 0;
    const update = async () => {
      const run = ++latest;
      const current = () => observing && run === latest;
      const seen = await readSeen(installId);
      if (!current()) return;
      let unread = false;
      for (const [index, conversation] of conversations.entries()) {
        if (!current()) return;
        const foreground = document.visibilityState === "visible";
        const open = visibleConversation();
        const room = `assistant:${installId}:${conversation.id}`;
        if (foreground && open?.installId === installId && open.conversation === conversation.id) {
          toast.clear(room);
          if (conversation.replySeq > 0) {
            await writeSeen(installId, conversation.id, conversation.replySeq);
            if (!current()) return;
          }
          continue;
        }
        if (!isUnread(seen, conversation.id, conversation.replySeq)) {
          toast.clear(room);
          continue;
        }
        unread = true;
        if (!foreground) continue;
        // The store invokes this inside its serialized transaction after the
        // fresh seen/announced/visibility checks; deciding and showing here
        // leaves no async gap in which a reservation could be abandoned.
        const delivered = await announceReply(installId, conversation.id, conversation.replySeq, () => {
          if (!mounted.current) return null;
          if (document.visibilityState !== "visible") return null;
          const openNow = visibleConversation();
          if (openNow?.installId === installId && openNow.conversation === conversation.id) return null;
          const query = new URLSearchParams({ conversation: conversation.id });
          toast.show("Assistant replied", room, {
            href: `/app-installations/${encodeURIComponent(installId)}?${query.toString()}`,
            note: conversationLabel(conversation, index),
          });
          return () => toast.clear(room);
        });
        if (!delivered) continue;
      }
      if (current()) setAssistantUnread(installId, unread);
    };
    const stop = onSeenChanged(installId, () => { void update(); });
    return () => {
      observing = false;
      stop();
    };
  }, [conversations, installId, toast, documentVisible]);
  return null;
}

/** One observed list per installed app; ownership reads also resolve queued View. */
export function AssistantUnreadProbes({ apps }: { apps: { installId: string }[] | null }) {
  useEffect(() => {
    setKnownInstalls(apps === null ? [] : apps.map((app) => app.installId));
  }, [apps]);
  if (apps === null) return null;
  return <>{apps.map((app) => <UnreadProbe key={app.installId} installId={app.installId} />)}</>;
}
