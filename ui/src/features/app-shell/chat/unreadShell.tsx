import { useEffect, useRef, useState } from "react";
import { useResource } from "../../../lib/useResource";
import { resources } from "../../../lib/resources";
import { conversationList } from "../conversationClient";
import { setAssistantUnread } from "./assistantUnread";
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

/** Mounted once by the board shell: one probe per installed app. */
export function AssistantUnreadProbes({ apps }: { apps: { installId: string }[] | null }) {
  const master = useResource(resources.masterState);
  const turn = master.data?.turn ?? null;
  // The conversation a turn just FINISHED on (edge), which is a reply even
  // before the list's `updated` is observed.
  const previous = useRef<string | null | undefined>(undefined);
  const finished = previous.current !== undefined && previous.current !== null && turn === null ? previous.current : null;
  useEffect(() => {
    previous.current = turn === null ? null : (turn.conversation ?? null);
  }, [turn]);
  if (apps === null) return null;
  return (
    <>
      {apps.map((app) => (
        <UnreadProbe key={app.installId} installId={app.installId} finished={finished} />
      ))}
    </>
  );
}
