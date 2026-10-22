import { useEffect } from "react";
import { subscribeMasterChanged } from "../../../lib/masterChanged";
import { resources } from "../../../lib/resources";
import { useQuery } from "../../../lib/useResource";
import type { ThreadEntry } from "../../home/thread";
import { useNotificationToast } from "../../threads/NotificationToast";
import { setAssistantUnread } from "./assistantUnread";
import { announceReply, isUnread, onSeenChanged, readSeen, writeSeen } from "./unreadSeen";
import { useDocumentVisible } from "./chatVisibility";

const HOME = "home";

/** Home is on screen and has shown everything up to `seq`. */
export function noteHomeSeen(seq: number): void {
  if (document.visibilityState !== "visible") return;
  writeSeen(HOME, HOME, seq);
  setAssistantUnread(HOME, false);
}

function newestReply(entries: ThreadEntry[]): number {
  let newest = 0;
  for (const entry of entries) {
    if (entry.role === "agent" &&
        (entry.kind === "assistant_text" || entry.kind === "turn_result") &&
        entry.text.trim() !== "" && entry.seq > newest) newest = entry.seq;
  }
  return newest;
}

/** The shell observes the same Home thread as the pane: master frames refresh
 * it without polling. No installation lookup is needed for a Home reply. */
export function useHomeUnread(onHome: boolean): void {
  const thread = useQuery(resources.masterThread);
  const entries = thread.data?.entries;
  const toast = useNotificationToast();
  const documentVisible = useDocumentVisible();
  useEffect(() => subscribeMasterChanged(() => resources.masterThread.invalidate()), []);
  useEffect(() => {
    const update = () => {
      const seq = newestReply(entries ?? []);
      const foreground = documentVisible && document.visibilityState === "visible";
      if (onHome && foreground) {
        toast.clear("assistant:home");
        noteHomeSeen(seq);
        return;
      }
      const unread = isUnread(readSeen(HOME), HOME, seq);
      setAssistantUnread(HOME, unread);
      if (!unread) toast.clear("assistant:home");
      if (foreground && unread && announceReply(HOME, HOME, seq)) {
        toast.show("Assistant replied", "assistant:home", { href: "/", note: "Home" });
      }
    };
    update();
    return onSeenChanged(HOME, update);
  }, [entries, onHome, toast, documentVisible]);
}
