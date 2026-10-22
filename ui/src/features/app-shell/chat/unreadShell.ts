import { useEffect, useRef } from "react";
import { useResource } from "../../../lib/useResource";
import { resources } from "../../../lib/resources";
import { conversationList } from "../conversationClient";
import { setAssistantUnread } from "./assistantUnread";
import { isUnread, readSeen } from "./unreadSeen";

/**
 * CAD-1700: the app reply dots, observed for the shell's whole lifetime.
 * A mounted app pane cannot own this: it unmounts when the operator goes
 * to Home, Issues or another app, which is exactly when a reply lands.
 * The shell recomputes from each install's cached conversation list on
 * every `master` frame (the frame already refreshed the observed lists)
 * and when another tab writes the seen map. A finished turn marks its own
 * install unread even before its list catches up.
 */
export function useAssistantUnreadShell(apps: { installId: string }[] | null): void {
  const master = useResource(resources.masterState);
  const previousTurn = useRef<{ conversation: string | null } | null | undefined>(undefined);

  useEffect(() => {
    const turn = master.data?.turn ?? null;
    const before = previousTurn.current;
    previousTurn.current = turn === null ? null : { conversation: turn.conversation ?? null };
    if (apps === null) return;

    const finished =
      before != null && turn === null ? (before.conversation ?? null) : null;
    let finishedOwner: string | null = null;
    if (finished !== null) {
      finishedOwner =
        apps.find((app) =>
          conversationList(app.installId)
            .get()
            .data?.conversations.some((c) => c.id === finished),
        )?.installId ?? null;
    }

    for (const app of apps) {
      const list = conversationList(app.installId).get().data;
      if (!list || list.legacy) continue;
      const seen = readSeen(app.installId);
      const unread = list.conversations.some((c) => isUnread(seen, c.id, c.updated));
      setAssistantUnread(app.installId, unread || app.installId === finishedOwner);
    }

    const onStorage = (event: StorageEvent) => {
      if (event.key === null || !event.key.startsWith("cadence.assistant.seen")) return;
      // The writing tab cleared a conversation: recompute from what is cached.
      for (const app of apps) {
        const list = conversationList(app.installId).get().data;
        if (!list || list.legacy) continue;
        const seen = readSeen(app.installId);
        setAssistantUnread(app.installId, list.conversations.some((c) => isUnread(seen, c.id, c.updated)));
      }
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, [master.data, apps]);
}
