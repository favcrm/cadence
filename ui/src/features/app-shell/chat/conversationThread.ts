import { useEffect } from "react";
import type { Resource } from "../../../lib/cache";
import { streamInto, type SseErrorState } from "../../../lib/sse";
import { resources } from "../../../lib/resources";
import type { MasterState } from "../../../lib/types";
import { EMPTY_THREAD, applyEarlier, fetchEarlier, reduceFrame, resumeSeq, threadItems, type PageReader, type ThreadPageLike, type ThreadState } from "../../home/thread";

/**
 * The one conversation thread engine Home and every app pane share
 * (CAD-1700): the live entry stream with resume, and older-page paging.
 * Each surface keeps only what is genuinely its own (Home's dock and
 * plan cards; the pane's picker, directives and visit fences) and its
 * scroll-follow machine, which already runs on the shared
 * `scrollFollow` predicates.
 */

/**
 * Stream `url` into `store` while `enabled`. Resumes after the store's
 * newest seq (never a replay); on reconnect `streamInto` resumes with
 * `?after=` and the reducer dedupes by seq. The server's first `master`
 * snapshot on every connection makes the bar live before replayed entries;
 * TCP open alone cannot enable actions against a cached previous turn.
 */
export function useThreadStream(
  store: Resource<ThreadState>,
  url: string | null,
  enabled: boolean,
  onLink: (state: "live" | SseErrorState) => void,
  onSnapshot?: () => void,
): void {
  useEffect(() => {
    if (!enabled || url === null) {
      onLink("live");
      return;
    }
    onLink("reconnecting");
    const sub = streamInto(store, (data, event) => {
      if (event.type === "master") {
        const snapshot = JSON.parse(event.data) as MasterState;
        if (Object.hasOwn(snapshot, "actor_identity")) {
          const conversation = snapshot.actor_identity?.conversation_id;
          if (typeof conversation !== "string" || conversation === "" || conversation !== data?.conversation) {
            onLink("reconnecting");
            return data ?? EMPTY_THREAD;
          }
          // A connection snapshot can arrive after a newer identity GET.
          // Reconcile through the canonical reader instead of rolling back
          // its opaque revision with an older streamed profile.
          void resources.masterConversation(conversation).refresh();
          if (resources.masterThread.get().data?.conversation === conversation) {
            resources.masterState.write(() => snapshot);
          }
        } else {
          resources.masterState.write(() => snapshot);
        }
        onSnapshot?.();
        onLink("live");
        return data ?? EMPTY_THREAD;
      }
      return reduceFrame(data, event);
    }, {
      url,
      events: ["master", "entry"],
      // Resume from what the daemon has SCANNED, not from the newest held
      // entry: a projection can hide rows past the last kept seq, and
      // resuming below them would re-request the same hidden page on every
      // (re)connect.
      lastEventId: String(resumeSeq(store.get().data) ?? 0),
      onOpen: () => onLink("reconnecting"),
      onError: (state) => {
        onLink(state);
        if (state === "stopped") void store.refresh();
      },
    });
    return () => sub.close();
    // Both callbacks are stable at the call sites; the stream must not
    // reopen when a parent re-renders.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [store, url, enabled]);
}

/** What one "Load earlier" press does. */
export type EarlierStep =
  /** Items already held are hidden by the window: widen it, no fetch. */
  | { kind: "widen" }
  /** Nothing older exists: the store is marked complete. */
  | { kind: "complete" }
  /** An older page is on its way; merge it with `land`. */
  | { kind: "fetch"; page: Promise<ThreadPageLike>; land: (older: ThreadPageLike) => void };

/**
 * One "Load earlier" step for `store` showing `limit` items: widen the
 * window over items already held, else fetch the page before the oldest
 * held entry. The caller owns its window size, loading flag and any
 * visit fencing; `land` merges the page into its own store regardless.
 */
export function earlierStep(store: Resource<ThreadState>, reader: PageReader, limit: number): EarlierStep | null {
  const data = store.get().data;
  if (!data) return null;
  if (threadItems(data).length > limit) return { kind: "widen" };
  const page = fetchEarlier(reader, data);
  if (!page) {
    store.write((cur) => ({ ...(cur ?? data), moreBefore: false }));
    return { kind: "complete" };
  }
  return { kind: "fetch", page, land: (older) => store.write((cur) => applyEarlier(cur, older)) };
}
