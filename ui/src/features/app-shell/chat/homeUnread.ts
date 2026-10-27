import { useEffect, useLayoutEffect, useRef } from "react";
import { subscribeMasterChanged } from "../../../lib/masterChanged";
import { resources } from "../../../lib/resources";
import { useQuery } from "../../../lib/useResource";
import { useNotificationToast } from "../../threads/NotificationToast";
import { setAssistantUnread } from "./assistantUnread";
import { announceReply, isUnread, onSeenChanged, readSeen, writeSeen } from "./unreadSeen";
import { useDocumentVisible } from "./chatVisibility";

const HOME = "home";

/** Home is on screen and has shown everything up to `seq`.
 *
 * The write is asynchronous and commits only while the tab is visible, so a
 * tab hidden before the transaction lands consumes nothing. The dot clears
 * only once the persisted watermark actually covers `seq`. */
export function noteHomeSeen(seq: number): void {
  if (document.visibilityState !== "visible" || !Number.isSafeInteger(seq) || seq <= 0) return;
  void writeSeen(HOME, HOME, seq).then(async (advanced) => {
    const seen = advanced ? seq : ((await readSeen(HOME)).home ?? 0);
    if (document.visibilityState === "visible" && seen >= seq) setAssistantUnread(HOME, false);
  });
}

/** The shell observes the same Home thread as the pane: master frames refresh
 * it without polling. The unread signal is the server's complete reply
 * watermark (`thread.data.replySeq`), never the bounded transcript tail: a
 * cold load shows only the newest page, and 200 non-reply entries after a
 * reply would hide it from any tail scan.
 *
 * Async observer runs are fenced: a slow read from an older run cannot clear
 * a room or a dot that a newer watermark still owns. A winning announcement
 * reservation is re-checked against fresh seen state before display, but not
 * against its own run: reservations do not wake observers, and an unrelated
 * commit must not silence a toast already won. */
export function useHomeUnread(onHome: boolean): void {
  const thread = useQuery(resources.masterThread);
  const replySeq = thread.data?.replySeq ?? 0;
  const toast = useNotificationToast();
  const documentVisible = useDocumentVisible();
  // Effect re-runs are not unmounts: a reservation won just before a list
  // refresh may still be displayed by this still-mounted observer.
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  // Home can open while a reservation is in flight; the latest route decides
  // whether the reserved toast still belongs to an offscreen room.
  const onHomeNow = useRef(onHome);
  useLayoutEffect(() => { onHomeNow.current = onHome; }, [onHome]);
  useEffect(() => subscribeMasterChanged(() => resources.masterThread.invalidate()), []);
  useEffect(() => {
    let observing = true;
    let latest = 0;
    const update = async () => {
      const run = ++latest;
      const current = () => observing && run === latest;
      const seen = await readSeen(HOME);
      if (!current()) return;
      const foreground = documentVisible && document.visibilityState === "visible";
      if (onHome && foreground) {
        toast.clear("assistant:home");
        noteHomeSeen(replySeq);
        setAssistantUnread(HOME, false);
        return;
      }
      const unread = isUnread(seen, HOME, replySeq);
      setAssistantUnread(HOME, unread);
      if (!unread) {
        toast.clear("assistant:home");
        return;
      }
      if (!foreground) return;
      // The store invokes this inside its serialized transaction after fresh
      // seen/announced/visibility checks; no post-reservation yield remains.
      const delivered = await announceReply(HOME, HOME, replySeq, () => {
        if (!mounted.current) return null;
        if (onHomeNow.current) return null;
        if (document.visibilityState !== "visible") return null;
        toast.show("Assistant replied", "assistant:home", { href: "/", note: "Home" });
        return () => toast.clear("assistant:home");
      });
      if (!delivered) return;
    };
    const stop = onSeenChanged(HOME, () => { void update(); });
    return () => {
      observing = false;
      stop();
    };
  }, [onHome, replySeq, toast, documentVisible]);
}
