import { useEffect } from "react";
import { useResource } from "../../../lib/useResource";
import { useNotificationToast } from "../../threads/NotificationToast";
import { conversationLabel, conversationList } from "../conversationClient";
import { setAssistantUnread } from "./assistantUnread";
import { visibleConversation, setKnownInstalls } from "./chatVisibility";
import { announceReply, isUnread, onSeenChanged, readSeen, writeSeen } from "./unreadSeen";

/** Observe real reply watermarks for the shell's lifetime, even when the app
 * pane is unmounted. An operator send, tool step or empty conversation never
 * advances this watermark. List reads refresh on the existing master frames. */
function UnreadProbe({ installId }: { installId: string }) {
  const list = useResource(conversationList(installId));
  const conversations = list.data?.legacy === false ? list.data.conversations : null;
  const toast = useNotificationToast();
  useEffect(() => {
    if (conversations === null) return;
    const update = () => {
      const seen = readSeen(installId);
      const open = visibleConversation();
      let unread = false;
      conversations.forEach((conversation, index) => {
        if (open?.installId === installId && open.conversation === conversation.id) {
          writeSeen(installId, conversation.id, conversation.replySeq);
          return;
        }
        if (!isUnread(seen, conversation.id, conversation.replySeq)) return;
        unread = true;
        if (announceReply(installId, conversation.id, conversation.replySeq)) {
          const query = new URLSearchParams({ conversation: conversation.id });
          toast.show("Assistant replied", `assistant:${installId}:${conversation.id}`, {
            href: `/app-installations/${encodeURIComponent(installId)}?${query.toString()}`,
            note: conversationLabel(conversation, index),
          });
        }
      });
      setAssistantUnread(installId, unread);
    };
    update();
    return onSeenChanged(installId, update);
  }, [conversations, installId, toast]);
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
